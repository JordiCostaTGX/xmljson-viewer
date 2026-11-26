use crate::{
    document::{self, Document, Kind},
    editor::Editor,
    settings::{self, Settings},
    theme::Theme,
    tree::Tree,
    update::{CURRENT_VERSION, Release},
};
use eframe::egui::{self, Align, Id, Key, KeyboardShortcut, Layout, Modifiers, ViewportCommand};
use std::{
    ffi::OsString,
    fs, io,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[cfg(test)]
#[path = "ui_tests.rs"]
mod tests;

const APP_TITLE: &str = "XML/JSON Viewer";
pub const APP_ID: &str = "io.github.xmljsonviewer.Viewer";
const DEBOUNCE: Duration = Duration::from_millis(350);
const SEARCH_ID: &str = "search-input";
const REPLACE_ID: &str = "replace-input";
const TREE_MIN_WIDTH: f32 = 160.0;

#[cfg(target_os = "windows")]
fn set_native_titlebar(
    window: &impl raw_window_handle::HasWindowHandle,
    palette: &crate::theme::Palette,
) {
    use raw_window_handle::RawWindowHandle;
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute,
    };

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = handle.hwnd.get() as _;
    let enabled = i32::from(palette.dark);
    // COLORREF is 0x00BBGGRR.
    let colorref = |color: egui::Color32| {
        u32::from(color.r()) | (u32::from(color.g()) << 8) | (u32::from(color.b()) << 16)
    };
    let caption_color = colorref(palette.panel);
    let text_color = colorref(palette.text);
    unsafe {
        let result = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            (&enabled as *const i32).cast(),
            std::mem::size_of::<i32>() as u32,
        );
        if result < 0 {
            DwmSetWindowAttribute(
                hwnd,
                19,
                (&enabled as *const i32).cast(),
                std::mem::size_of::<i32>() as u32,
            );
        }
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_CAPTION_COLOR as u32,
            (&caption_color as *const u32).cast(),
            std::mem::size_of::<u32>() as u32,
        );
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_TEXT_COLOR as u32,
            (&text_color as *const u32).cast(),
            std::mem::size_of::<u32>() as u32,
        );
    }
}

const ARCHIVE_JOB: u64 = u64::MAX;

enum Task {
    Parse(Arc<str>, Kind),
    Open(PathBuf),
    ListZip(PathBuf),
    OpenZip {
        path: PathBuf,
        indices: Vec<usize>,
        /// Entries the listing already skipped, reported together with any skipped now.
        skipped: usize,
    },
    Format(Arc<str>, Kind),
    /// Selected text, whole source, selected char range, and whether it was triggered by opening.
    Decode(String, String, usize, usize, bool),
    Save(PathBuf, String),
}

struct Job {
    tab: u64,
    revision: u64,
    current: Arc<AtomicU64>,
    task: Task,
}

struct OpenedDocument {
    path: Option<PathBuf>,
    name: String,
    kind: Kind,
    parsed: Result<Arc<Document>, String>,
    text: Option<Arc<str>>,
}

struct OpenedSet {
    documents: Vec<OpenedDocument>,
    notice: String,
}

struct ZipListing {
    path: PathBuf,
    entries: Vec<crate::archive::ListedEntry>,
    skipped: usize,
}

enum Output {
    Parsed(Result<Arc<Document>, String>),
    Opened(Result<OpenedSet, String>),
    Listed(Result<ZipListing, String>),
    Formatted(Result<String, String>),
    Decoded(Result<Option<String>, String>, usize, usize, bool),
    Saved(PathBuf, Result<(), String>),
}

struct Reply {
    tab: u64,
    revision: u64,
    output: Output,
}

fn replace_char_range(source: &str, start: usize, end: usize, replacement: &str) -> String {
    let start_byte = source
        .char_indices()
        .nth(start)
        .map_or(source.len(), |(byte, _)| byte);
    let end_byte = source
        .char_indices()
        .nth(end)
        .map_or(source.len(), |(byte, _)| byte);
    format!(
        "{}{}{}",
        &source[..start_byte],
        replacement,
        &source[end_byte..]
    )
}

fn worker(notify: impl Fn() + Send + 'static) -> (mpsc::Sender<Job>, mpsc::Receiver<Reply>) {
    let (send, jobs) = mpsc::channel::<Job>();
    let (replies, receive) = mpsc::channel();
    std::thread::spawn(move || {
        for job in jobs {
            let cancelled = || job.current.load(Ordering::Relaxed) != job.revision;
            if cancelled() && !matches!(job.task, Task::Save(..)) {
                continue;
            }
            let output = match job.task {
                Task::Parse(text, kind) => {
                    Output::Parsed(document::parse(text, kind, cancelled).map(Arc::new))
                }
                Task::Open(path) => Output::Opened(open_path(&path, &cancelled)),
                Task::ListZip(path) => Output::Listed(list_zip(&path, &cancelled)),
                Task::OpenZip {
                    path,
                    indices,
                    skipped,
                } => Output::Opened(open_zip(&path, &indices, skipped, &cancelled)),
                Task::Format(text, kind) => Output::Formatted(document::format(&text, kind)),
                Task::Decode(selected, source, start, end, on_open) => {
                    let source_len = source.chars().count();
                    let decoded = document::decode(&selected).and_then(|decoded| {
                        decoded
                            .map(|text| {
                                let source = replace_char_range(&source, start, end, &text);
                                document::format(&source, Kind::detect(&source))
                            })
                            .transpose()
                    });
                    Output::Decoded(decoded, 0, source_len, on_open)
                }
                Task::Save(path, text) => {
                    let result =
                        write_atomic(&path, &text).map_err(|e| format!("Could not save file: {e}"));
                    Output::Saved(path, result)
                }
            };
            let reply = Reply {
                tab: job.tab,
                revision: job.revision,
                output,
            };
            if replies.send(reply).is_err() {
                break;
            }
            notify();
        }
    });
    (send, receive)
}

fn list_zip(path: &Path, cancelled: &impl Fn() -> bool) -> Result<ZipListing, String> {
    let listed = crate::archive::list_text_files(path, cancelled)?;
    Ok(ZipListing {
        path: path.to_path_buf(),
        entries: listed.entries,
        skipped: listed.skipped,
    })
}

fn open_zip(
    path: &Path,
    indices: &[usize],
    skipped: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<OpenedSet, String> {
    let read = crate::archive::read_entries(path, indices, cancelled)?;
    let notice = crate::archive::notice(skipped + read.skipped, read.truncated);
    let documents = read
        .entries
        .into_iter()
        .map(|entry| {
            let forces_json = Path::new(&entry.name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
            prepare_document(None, entry.name, entry.text, forces_json, cancelled)
        })
        .collect();
    Ok(OpenedSet { documents, notice })
}

fn open_path(path: &Path, cancelled: &impl Fn() -> bool) -> Result<OpenedSet, String> {
    match fs::read_to_string(path) {
        Ok(text) => {
            let forces_json = path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Untitled".into());
            Ok(OpenedSet {
                documents: vec![prepare_document(
                    Some(path.to_path_buf()),
                    name,
                    text,
                    forces_json,
                    cancelled,
                )],
                notice: String::new(),
            })
        }
        Err(error) => Err(format!("Could not open file: {error}")),
    }
}

fn prepare_document(
    path: Option<PathBuf>,
    name: String,
    text: String,
    forces_json: bool,
    cancelled: &impl Fn() -> bool,
) -> OpenedDocument {
    let kind = if forces_json {
        Kind::Json
    } else {
        Kind::detect(&text)
    };
    let text: Arc<str> = text.into();
    let parsed = document::parse(text.clone(), kind, cancelled).map(Arc::new);
    OpenedDocument {
        path,
        name,
        kind,
        parsed,
        text: Some(text),
    }
}

/// Writes to a sibling temporary file and renames it over the target.
fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing file name"))?;
    let mut temporary = OsString::from(".");
    temporary.push(name);
    temporary.push(format!(".{}.tmp", std::process::id()));
    let temporary = path.with_file_name(temporary);
    let create = || {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
    };
    let result = (|| {
        let mut file = match create() {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                // A stale file from a crashed save; removing it never follows a symlink.
                fs::remove_file(&temporary)?;
                create()?
            }
            other => other?,
        };
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

struct Tab {
    id: u64,
    editor: Editor,
    seen_version: u64,
    tree: Tree,
    document: Option<Arc<Document>>,
    document_revision: u64,
    revision: Arc<AtomicU64>,
    debounce: Option<Instant>,
    kind: Option<Kind>,
    path: Option<PathBuf>,
    untitled_name: String,
    dirty: bool,
    saving: bool,
    close_after_save: bool,
    status: String,
    parse_count: usize,
    parse_error: Option<String>,
    /// Entries skipped while opening a zip, kept for the status after decode on open.
    open_notice: String,
}

impl Tab {
    fn new(id: u64) -> Self {
        let editor = Editor::default();
        Self {
            id,
            seen_version: editor.version(),
            editor,
            tree: Tree::default(),
            document: None,
            document_revision: u64::MAX,
            revision: Arc::new(AtomicU64::new(0)),
            debounce: None,
            kind: None,
            path: None,
            untitled_name: "Untitled".into(),
            dirty: false,
            saving: false,
            close_after_save: false,
            status: "Ready".into(),
            parse_count: 0,
            parse_error: None,
            open_notice: String::new(),
        }
    }

    fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    fn title(&self) -> String {
        self.path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.untitled_name.clone())
    }

    /// Zip entries are titled with their folder inside the archive; only the last part is a file name.
    fn suggested_file_name(&self) -> String {
        let title = self.title();
        title
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&title)
            .to_string()
    }

    fn set_kind(&mut self, kind: Kind) {
        self.kind = Some(kind);
        self.editor.set_kind(kind);
    }

    fn tree_current(&self) -> bool {
        self.document.is_some() && self.document_revision == self.revision()
    }

    /// Records edits made since the last check and restarts the analysis timer.
    fn note_changes(&mut self, now: Instant) {
        if self.editor.version() != self.seen_version {
            self.seen_version = self.editor.version();
            self.revision.fetch_add(1, Ordering::Relaxed);
            self.dirty = true;
            self.debounce = Some(now + DEBOUNCE);
            self.status = "Changes pending analysis…".into();
        }
    }

    fn install_document(&mut self, revision: u64, parsed: Result<Arc<Document>, String>) {
        self.parse_count += 1;
        match parsed {
            Ok(document) => {
                self.tree.set_document(document.clone());
                self.status = format!("{} nodes · analysis up to date", document.nodes.len());
                self.document = Some(document);
                self.document_revision = revision;
                self.parse_error = None;
            }
            Err(error) => {
                self.parse_error = Some(error.clone());
                self.status = error;
            }
        }
    }

    fn select_node(&mut self, id: usize) {
        let Some(document) = self.document.clone() else {
            return;
        };
        let range = &document.nodes[id].chars;
        self.editor
            .select_range(range.start as usize, range.end as usize);
        self.status = format!("Selected: {}", document.nodes[id].label);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    New,
    NewWindow,
    MoveToNewWindow(u64),
    Open,
    OpenEmbedded { tab: u64, node: usize },
    Save,
    SaveAs,
    CloseActive,
    Close(u64),
    Activate(usize),
    Quit,
    Select,
    Reveal,
    Format,
    Search,
    Replace,
    ReplaceNext,
    ReplaceAll,
    Next,
    Previous,
    HideSearch,
    Undo,
    Redo,
    SelectAll,
    ParseXml,
    ParseJson,
    ExpandAll,
    CollapseAll,
    CheckForUpdates,
    Decode,
}

enum Dialog {
    CloseTab(u64),
    Quit,
    Zip(ZipPrompt),
}

struct ZipPrompt {
    path: PathBuf,
    names: Vec<String>,
    indices: Vec<usize>,
    skipped: usize,
    picking: bool,
    selected: Vec<bool>,
}

enum ZipAction {
    All,
    Pick,
    Selected,
    Back,
    Cancel,
}

pub struct App {
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    jobs: mpsc::Sender<Job>,
    replies: mpsc::Receiver<Reply>,
    open_requests: Option<mpsc::Receiver<Vec<PathBuf>>>,
    search_visible: bool,
    search_query: String,
    search_case_sensitive: bool,
    decode_on_open: bool,
    replace_visible: bool,
    replace_text: String,
    focus_search: bool,
    focus_editor: bool,
    dialog: Option<Dialog>,
    archive_token: Arc<AtomicU64>,
    closing: bool,
    quit_after_save: bool,
    title: String,
    tree_width: f32,
    tree_resized: bool,
    open_files_in_new_window: bool,
    persist_settings: bool,
    theme: Theme,
    applied_theme: Option<Theme>,
    repaint: Option<egui::Context>,
    check_for_updates: bool,
    skipped_version: String,
    update_available: Option<Release>,
    /// "Later" hides the notice for this session only.
    update_dismissed: bool,
    update_replies: Option<mpsc::Receiver<UpdateCheck>>,
}

struct UpdateCheck {
    result: Result<Release, String>,
    manual: bool,
}

fn spawn_window(files: &[PathBuf]) -> io::Result<()> {
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command.arg(crate::NEW_WINDOW_FLAG);
    if !files.is_empty() {
        command.arg("--").args(files);
    }
    let mut child = command.spawn()?;
    // Reap the child when it exits so it does not linger as a zombie on Unix.
    std::thread::spawn(move || child.wait());
    Ok(())
}

fn file_dialog(frame: Option<&eframe::Frame>) -> rfd::FileDialog {
    let dialog = rfd::FileDialog::new()
        .add_filter("XML, JSON and ZIP", &["xml", "json", "zip"])
        .add_filter("All files", &["*"]);
    match frame {
        Some(frame) => dialog.set_parent(frame),
        None => dialog,
    }
}

impl App {
    fn new(ctx: Option<egui::Context>) -> Self {
        let repaint = ctx.clone();
        let (jobs, replies) = worker(move || {
            if let Some(ctx) = &ctx {
                ctx.request_repaint();
            }
        });
        let mut app = Self {
            tabs: vec![],
            active: 0,
            next_id: 0,
            jobs,
            replies,
            open_requests: None,
            search_visible: false,
            search_query: String::new(),
            search_case_sensitive: true,
            decode_on_open: false,
            replace_visible: false,
            replace_text: String::new(),
            focus_search: false,
            focus_editor: true,
            dialog: None,
            archive_token: Arc::new(AtomicU64::new(0)),
            closing: false,
            quit_after_save: false,
            title: String::new(),
            tree_width: settings::DEFAULT_TREE_WIDTH,
            tree_resized: false,
            open_files_in_new_window: false,
            persist_settings: false,
            theme: Theme::default(),
            applied_theme: None,
            repaint,
            check_for_updates: true,
            skipped_version: String::new(),
            update_available: None,
            update_dismissed: false,
            update_replies: None,
        };
        app.new_tab();
        app
    }

    fn apply_settings(&mut self, settings: Settings) {
        self.tree_width = settings.tree_width.max(TREE_MIN_WIDTH);
        self.open_files_in_new_window = settings.open_files_in_new_window;
        self.search_case_sensitive = settings.search_case_sensitive;
        self.decode_on_open = settings.decode_on_open;
        self.theme = Theme::from_id(&settings.theme);
        self.check_for_updates = settings.check_for_updates;
        self.skipped_version = settings.skipped_version;
        self.offer_update(
            Release {
                version: settings.latest_version,
            },
            false,
        );
        self.persist_settings = true;
    }

    /// Runs off the UI thread; `manual` checks report their outcome in the status bar.
    fn start_update_check(&mut self, manual: bool) {
        if self.update_replies.is_some() {
            return;
        }
        let (send, receive) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let result = crate::update::fetch_latest();
            let _ = send.send(UpdateCheck { result, manual });
            if let Some(ctx) = repaint {
                ctx.request_repaint();
            }
        });
        self.update_replies = Some(receive);
        if manual {
            self.tabs[self.active].status = "Checking for updates…".into();
        }
    }

    fn poll_update_check(&mut self) {
        let Some(receiver) = &self.update_replies else {
            return;
        };
        let check = match receiver.try_recv() {
            Ok(check) => check,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.update_replies = None;
                return;
            }
        };
        self.update_replies = None;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        match check.result {
            Ok(release) => {
                if self.persist_settings {
                    let version = release.version.clone();
                    settings::update(|settings| {
                        settings.last_update_check = now;
                        settings.latest_version = version;
                    });
                }
                if check.manual {
                    self.tabs[self.active].status =
                        if crate::update::is_newer(&release.version, CURRENT_VERSION) {
                            format!("Version {} is available", release.version)
                        } else {
                            format!("You have the latest version ({CURRENT_VERSION})")
                        };
                }
                self.offer_update(release, check.manual);
            }
            Err(error) => {
                // Also recorded on failure, so an offline PC does not retry on every launch.
                if self.persist_settings {
                    settings::update(|settings| settings.last_update_check = now);
                }
                crate::diagnostics::record(&format!("Update check failed: {error}"));
                if check.manual {
                    self.tabs[self.active].status = format!("Could not check for updates: {error}");
                }
            }
        }
    }

    /// A skipped version is only offered again when the user checks manually.
    fn offer_update(&mut self, release: Release, manual: bool) {
        if !crate::update::is_newer(&release.version, CURRENT_VERSION) {
            self.update_available = None;
        } else if manual || release.version != self.skipped_version {
            self.update_dismissed = false;
            self.update_available = Some(release);
        }
    }

    fn move_to_new_window(&mut self, id: u64) {
        let Some(index) = self.index(id) else {
            return;
        };
        let tab = &mut self.tabs[index];
        let Some(path) = tab.path.clone().filter(|_| !tab.dirty && !tab.saving) else {
            tab.status = "Save the tab before moving it to a new window".into();
            return;
        };
        match spawn_window(&[path]) {
            Ok(()) => self.remove_tab(index),
            Err(error) => tab.status = format!("Could not open a new window: {error}"),
        }
    }

    fn new_tab(&mut self) -> usize {
        self.tabs.push(Tab::new(self.next_id));
        self.next_id += 1;
        self.active = self.tabs.len() - 1;
        self.focus_editor = true;
        self.active
    }

    fn index(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    fn submit(&mut self, index: usize, task: Task) {
        let tab = &mut self.tabs[index];
        let job = Job {
            tab: tab.id,
            revision: tab.revision(),
            current: tab.revision.clone(),
            task,
        };
        if self.jobs.send(job).is_err() {
            tab.status = "Error: parsing worker unavailable".into();
        }
    }

    fn parse(&mut self, index: usize, explicit: Option<Kind>) {
        let tab = &mut self.tabs[index];
        tab.debounce = None;
        let text: Arc<str> = tab.editor.text().into();
        let kind = explicit.or(tab.kind).unwrap_or_else(|| Kind::detect(&text));
        if explicit.is_some() {
            tab.revision.fetch_add(1, Ordering::Relaxed);
        }
        tab.set_kind(kind);
        tab.status = "Parsing…".into();
        self.submit(index, Task::Parse(text, kind));
    }

    fn open(&mut self, path: PathBuf) {
        if crate::archive::is_zip(&path) {
            self.queue_zip_listing(path);
            return;
        }
        self.open_file(path);
    }

    fn queue_zip_listing(&mut self, path: PathBuf) {
        let revision = self.archive_token.fetch_add(1, Ordering::Relaxed) + 1;
        let job = Job {
            tab: ARCHIVE_JOB,
            revision,
            current: self.archive_token.clone(),
            task: Task::ListZip(path),
        };
        if self.jobs.send(job).is_err() {
            self.tabs[self.active].status = "Error: parsing worker unavailable".into();
        } else {
            self.tabs[self.active].status = "Checking archive…".into();
        }
    }

    fn open_file(&mut self, path: PathBuf) {
        let reuse = self.tabs.get(self.active).is_some_and(|tab| {
            !tab.dirty && tab.path.is_none() && tab.editor.len_chars() == 0 && tab.editor.editable
        });
        let index = if reuse { self.active } else { self.new_tab() };
        let tab = &mut self.tabs[index];
        tab.revision.fetch_add(1, Ordering::Relaxed);
        tab.editor.editable = false;
        tab.status = format!("Opening {}…", path.display());
        self.submit(index, Task::Open(path));
    }

    fn present_zip(&mut self, result: Result<ZipListing, String>) {
        match result {
            Err(error) => self.tabs[self.active].status = error,
            Ok(listing) if listing.entries.is_empty() => {
                self.tabs[self.active].status = if listing.skipped == 0 {
                    "The archive has no text files".into()
                } else {
                    crate::archive::notice(listing.skipped, false)
                };
            }
            Ok(listing) => {
                let (indices, names): (Vec<usize>, Vec<String>) = listing
                    .entries
                    .into_iter()
                    .map(|entry| (entry.index, entry.name))
                    .unzip();
                self.dialog = Some(Dialog::Zip(ZipPrompt {
                    path: listing.path,
                    selected: vec![false; names.len()],
                    names,
                    indices,
                    skipped: listing.skipped,
                    picking: false,
                }));
            }
        }
    }

    fn open_confirmed_zip(&mut self, all: bool) {
        let Some(Dialog::Zip(prompt)) = self.dialog.take() else {
            return;
        };
        let (indices, skipped) = if all {
            (prompt.indices, prompt.skipped)
        } else {
            let chosen: Vec<usize> = prompt
                .indices
                .iter()
                .zip(&prompt.selected)
                .filter(|(_, selected)| **selected)
                .map(|(index, _)| *index)
                .collect();
            if chosen.is_empty() {
                self.dialog = Some(Dialog::Zip(prompt));
                return;
            }
            (chosen, 0)
        };
        self.open_zip_files(prompt.path, indices, skipped);
    }

    fn open_zip_files(&mut self, path: PathBuf, indices: Vec<usize>, skipped: usize) {
        let reuse = self.tabs.get(self.active).is_some_and(|tab| {
            !tab.dirty && tab.path.is_none() && tab.editor.len_chars() == 0 && tab.editor.editable
        });
        let index = if reuse { self.active } else { self.new_tab() };
        let tab = &mut self.tabs[index];
        tab.revision.fetch_add(1, Ordering::Relaxed);
        tab.editor.editable = false;
        tab.status = format!("Opening {}…", path.display());
        self.submit(
            index,
            Task::OpenZip {
                path,
                indices,
                skipped,
            },
        );
    }

    fn clear_archive_check_status(&mut self) {
        let tab = &mut self.tabs[self.active];
        if tab.status == "Checking archive…" {
            tab.status = "Ready".into();
        }
    }

    fn show_zip_dialog(&mut self, ctx: &egui::Context) {
        let mut action = None;
        let mut toggles = Vec::new();
        if let Some(Dialog::Zip(prompt)) = self.dialog.as_mut() {
            let count = prompt.names.len();
            let heading = if count == 1 {
                "This file contains 1 document.".to_string()
            } else {
                format!("This file contains {count} documents.")
            };
            let selected_count = prompt.selected.iter().filter(|chosen| **chosen).count();
            let response = egui::Modal::new(Id::unique("zip-open")).show(ctx, |ui| {
                ui.set_width(460.0);
                ui.heading(heading);
                if prompt.skipped > 0 {
                    ui.label(format!(
                        "Skipped {} entries that could not be opened as text",
                        prompt.skipped
                    ));
                }
                if count > crate::archive::MAX_FILES {
                    ui.label(format!(
                        "Open all keeps the first {}.",
                        crate::archive::MAX_FILES
                    ));
                }
                ui.add_space(8.0);
                if prompt.picking {
                    ui.label(format!("{selected_count} selected"));
                    egui::ScrollArea::vertical()
                        .max_height(280.0)
                        .show(ui, |ui| {
                            for (index, name) in prompt.names.iter().enumerate() {
                                let mut checked = prompt.selected[index];
                                let enabled = checked || selected_count < crate::archive::MAX_FILES;
                                if ui
                                    .add_enabled(enabled, egui::Checkbox::new(&mut checked, name))
                                    .changed()
                                {
                                    toggles.push((index, checked));
                                }
                            }
                        });
                    ui.add_space(8.0);
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add_enabled(selected_count > 0, egui::Button::new("Open selected"))
                            .clicked()
                        {
                            action = Some(ZipAction::Selected);
                        }
                        if ui.button("Back").clicked() {
                            action = Some(ZipAction::Back);
                        }
                        if ui.button("Cancel").clicked() {
                            action = Some(ZipAction::Cancel);
                        }
                    });
                } else {
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .button(format!("Open all (up to {})", crate::archive::MAX_FILES))
                            .clicked()
                        {
                            action = Some(ZipAction::All);
                        }
                        if ui.button("Select which to open").clicked() {
                            action = Some(ZipAction::Pick);
                        }
                        if ui.button("Cancel").clicked() {
                            action = Some(ZipAction::Cancel);
                        }
                    });
                }
            });
            for (index, checked) in toggles {
                prompt.selected[index] = checked;
            }
            if action.is_none() && response.should_close() {
                action = Some(ZipAction::Cancel);
            }
        }
        match action {
            Some(ZipAction::Pick) => {
                if let Some(Dialog::Zip(prompt)) = self.dialog.as_mut() {
                    prompt.picking = true;
                }
            }
            Some(ZipAction::Back) => {
                if let Some(Dialog::Zip(prompt)) = self.dialog.as_mut() {
                    prompt.picking = false;
                }
            }
            Some(ZipAction::Cancel) => {
                self.dialog = None;
                self.clear_archive_check_status();
            }
            Some(ZipAction::All) => self.open_confirmed_zip(true),
            Some(ZipAction::Selected) => self.open_confirmed_zip(false),
            None => {}
        }
    }

    fn open_embedded(&mut self, tab_id: u64, node_id: usize) {
        let Some(index) = self.index(tab_id) else {
            return;
        };
        let source = &mut self.tabs[index];
        source.note_changes(Instant::now());
        if !source.tree_current() {
            return;
        }
        let Some(node) = source
            .document
            .as_ref()
            .and_then(|doc| doc.nodes.get(node_id))
        else {
            return;
        };
        let Some(kind) = node.embedded_kind() else {
            return;
        };
        let raw = source
            .editor
            .slice(node.chars.start as usize..node.chars.end as usize);
        let Some(payload) = node.embedded_payload(&raw) else {
            source.status = match kind {
                Kind::Xml => "This node does not contain XML",
                Kind::Json => "This node does not contain a JSON object or array",
            }
            .into();
            return;
        };
        let index = self.new_tab();
        let tab = &mut self.tabs[index];
        tab.untitled_name = match kind {
            Kind::Xml => "Embedded XML",
            Kind::Json => "Embedded JSON",
        }
        .into();
        tab.editor.set_text(&payload);
        tab.seen_version = tab.editor.version();
        self.parse(index, Some(kind));
    }

    fn receive_open_requests(&mut self, ctx: &egui::Context) {
        let requests: Vec<_> = self
            .open_requests
            .as_ref()
            .map(|receiver| receiver.try_iter().collect())
            .unwrap_or_default();
        for files in requests {
            if files.is_empty() {
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            } else {
                for path in files {
                    self.open(path);
                }
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            }
        }
    }

    fn poll(&mut self) {
        // Limit work per frame so many completions cannot monopolize the UI.
        for _ in 0..8 {
            match self.replies.try_recv() {
                Ok(reply) => self.accept(reply),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.tabs[self.active].status = "Error: the parsing worker has stopped".into();
                    break;
                }
            }
        }
    }

    fn accept(&mut self, reply: Reply) {
        let Reply {
            tab: tab_id,
            revision,
            output,
        } = reply;
        let output = match output {
            Output::Listed(result) => {
                if revision == self.archive_token.load(Ordering::Relaxed) {
                    self.present_zip(result);
                }
                return;
            }
            other => other,
        };
        let Some(index) = self.index(tab_id) else {
            return;
        };
        let mut opened = None;
        {
            let tab = &mut self.tabs[index];
            if revision != tab.revision() && !matches!(output, Output::Saved(..)) {
                return;
            }
            match output {
                Output::Listed(_) => return,
                Output::Parsed(parsed) => tab.install_document(revision, parsed),
                Output::Opened(result) => opened = Some(result),
                Output::Formatted(Ok(text)) => {
                    let len = tab.editor.len_chars();
                    tab.editor.replace(0..len, &text);
                    tab.editor.select_range(0, 0);
                    tab.note_changes(Instant::now());
                    tab.status =
                    "Document formatted; mixed XML content is preserved without added whitespace"
                        .into();
                }
                Output::Formatted(Err(error)) => tab.status = format!("Formatting error: {error}"),
                Output::Decoded(Ok(Some(text)), start, end, on_open) => {
                    tab.editor.replace(start..end, &text);
                    tab.note_changes(Instant::now());
                    if on_open {
                        // Nothing on disk changed: the file or zip can be opened again.
                        tab.dirty = false;
                        tab.status = "Decoded on open; Ctrl+Z restores the original text".into();
                        if !tab.open_notice.is_empty() {
                            tab.status = format!("{} · {}", tab.status, tab.open_notice);
                        }
                    } else {
                        tab.status = "Selection decoded and document structured".into();
                    }
                }
                Output::Decoded(Ok(None), .., on_open) => {
                    if !on_open {
                        tab.status = "No escaped content to decode".into();
                    }
                }
                Output::Decoded(Err(error), ..) => tab.status = format!("Decode error: {error}"),
                Output::Saved(path, result) => {
                    tab.saving = false;
                    match result {
                        Ok(()) => {
                            tab.path = Some(path);
                            let unchanged = revision == tab.revision();
                            tab.dirty = !unchanged;
                            tab.status = if unchanged {
                                "Saved"
                            } else {
                                "Saved; newer edits are still pending"
                            }
                            .into();
                            if std::mem::take(&mut tab.close_after_save) && unchanged {
                                self.remove_tab(index);
                            }
                        }
                        Err(error) => {
                            tab.close_after_save = false;
                            tab.status = error;
                        }
                    }
                }
            }
        }
        if let Some(result) = opened {
            self.finish_open(index, result);
        }
    }

    fn finish_open(&mut self, index: usize, opened: Result<OpenedSet, String>) {
        let installed = match opened {
            Err(error) => {
                let tab = &mut self.tabs[index];
                tab.editor.editable = true;
                tab.status = error;
                return;
            }
            Ok(set) if set.documents.is_empty() => {
                let tab = &mut self.tabs[index];
                tab.editor.editable = true;
                tab.status = if set.notice.is_empty() {
                    "The archive has no text files".into()
                } else {
                    set.notice
                };
                return;
            }
            Ok(set) => {
                let notice = set.notice;
                let mut installed = Vec::new();
                for (offset, document) in set.documents.into_iter().enumerate() {
                    let tab_index = if offset == 0 { index } else { self.new_tab() };
                    self.install_opened(tab_index, document);
                    installed.push(tab_index);
                }
                if !notice.is_empty() {
                    let tab = &mut self.tabs[self.active];
                    tab.status = if tab.status.is_empty() {
                        notice.clone()
                    } else {
                        format!("{} · {notice}", tab.status)
                    };
                    tab.open_notice = notice;
                }
                installed
            }
        };
        if self.decode_on_open {
            for tab_index in installed {
                self.decode_opened_tab(tab_index);
            }
        }
    }

    fn install_opened(&mut self, index: usize, document: OpenedDocument) {
        let revision = self.tabs[index].revision();
        let tab = &mut self.tabs[index];
        tab.editor.editable = true;
        let OpenedDocument {
            path,
            name,
            kind,
            parsed,
            text,
        } = document;
        let Some(text) = text else {
            if let Err(error) = parsed {
                tab.status = error;
            }
            return;
        };
        tab.set_kind(kind);
        tab.editor.set_text(&text);
        tab.seen_version = tab.editor.version();
        tab.path = path;
        tab.dirty = false;
        if tab.path.is_none() {
            tab.untitled_name = name;
        }
        tab.install_document(revision, parsed);
    }

    fn decode_opened_tab(&mut self, index: usize) {
        let tab = &self.tabs[index];
        if tab.editor.len_chars() == 0 {
            return;
        }
        let source = tab.editor.text();
        let end = tab.editor.len_chars();
        self.submit(index, Task::Decode(source.clone(), source, 0, end, true));
    }

    /// Returns how long until the next pending analysis is due.
    fn process_changes(&mut self, now: Instant) -> Option<Duration> {
        let mut wake: Option<Duration> = None;
        let mut due = vec![];
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            tab.note_changes(now);
            if let Some(deadline) = tab.debounce {
                if deadline <= now {
                    due.push(index);
                } else {
                    let remaining = deadline - now;
                    wake = Some(wake.map_or(remaining, |w| w.min(remaining)));
                }
            }
        }
        for index in due {
            self.parse(index, None);
        }
        wake
    }

    fn select_at_cursor(&mut self, expand: bool) {
        let tab = &mut self.tabs[self.active];
        if !tab.tree_current() {
            tab.status = "Wait for parsing to finish before synchronizing the selection".into();
            return;
        }
        let Some(document) = tab.document.clone() else {
            return;
        };
        let cursor = i32::try_from(tab.editor.cursor()).unwrap_or(i32::MAX);
        let Some(id) = document.at_cursor(cursor) else {
            tab.status = "No element at this position".into();
            return;
        };
        tab.tree.reveal(id, expand);
        tab.select_node(id);
    }

    fn find(&mut self, backwards: bool) {
        let tab = &mut self.tabs[self.active];
        tab.editor
            .set_search(&self.search_query, self.search_case_sensitive);
        if !tab.editor.find(backwards) {
            tab.status = "No matches found".into();
        }
    }

    fn toggle_search(&mut self, replace: bool) {
        if self.search_visible && self.replace_visible == replace {
            self.search_visible = false;
            self.replace_visible = false;
            self.focus_editor = true;
        } else {
            self.search_visible = true;
            self.replace_visible = replace;
            self.focus_search = true;
        }
    }

    fn replace(&mut self, all: bool) {
        let tab = &mut self.tabs[self.active];
        if !tab.editor.editable {
            tab.status = "Wait for the file to finish opening".into();
            return;
        }
        tab.editor
            .set_search(&self.search_query, self.search_case_sensitive);
        if tab.editor.match_count() == 0 {
            tab.status = "No matches found".into();
        } else if all {
            let count = tab.editor.replace_all(&self.replace_text);
            tab.status = format!("Replaced {count} occurrence(s)");
        } else {
            tab.editor.replace_match(&self.replace_text);
        }
    }

    fn save(&mut self, save_as: bool, frame: Option<&eframe::Frame>) {
        let index = self.active;
        let tab = &mut self.tabs[index];
        if !tab.editor.editable {
            tab.status = "Wait for the file to finish opening".into();
            return;
        }
        match tab.path.clone() {
            Some(path) if !save_as => self.save_to(index, path),
            _ => self.save_with_dialog(index, frame),
        }
    }

    fn save_with_dialog(&mut self, index: usize, frame: Option<&eframe::Frame>) {
        let tab = &mut self.tabs[index];
        match file_dialog(frame)
            .set_title("Save as")
            .set_file_name(tab.suggested_file_name())
            .save_file()
        {
            Some(path) => self.save_to(index, path),
            None => tab.close_after_save = false,
        }
    }

    fn save_to(&mut self, index: usize, path: PathBuf) {
        let tab = &mut self.tabs[index];
        if tab.saving {
            tab.status = "A save is already in progress".into();
            return;
        }
        tab.saving = true;
        tab.status = "Saving…".into();
        let text = tab.editor.text();
        self.submit(index, Task::Save(path, text));
        let tab = &mut self.tabs[index];
        if tab.status.starts_with("Error") {
            tab.saving = false;
            tab.close_after_save = false;
        }
    }

    fn close_tab(&mut self, id: u64) {
        let Some(index) = self.index(id) else {
            return;
        };
        let tab = &mut self.tabs[index];
        if tab.saving {
            tab.status = "Wait for the save to finish".into();
        } else if tab.dirty {
            self.dialog = Some(Dialog::CloseTab(id));
        } else {
            self.remove_tab(index);
        }
    }

    fn remove_tab(&mut self, index: usize) {
        // Obsolete background work for this tab is cancelled and ignored.
        self.tabs[index].revision.fetch_add(1, Ordering::Relaxed);
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.new_tab();
        } else {
            if self.active > index {
                self.active -= 1;
            }
            self.active = self.active.min(self.tabs.len() - 1);
        }
    }

    fn perform(&mut self, action: Action, ctx: &egui::Context, frame: Option<&eframe::Frame>) {
        let index = self.active;
        match action {
            Action::New => {
                self.new_tab();
            }
            Action::NewWindow => {
                if let Err(error) = spawn_window(&[]) {
                    self.tabs[index].status = format!("Could not open a new window: {error}");
                }
            }
            Action::MoveToNewWindow(id) => self.move_to_new_window(id),
            Action::Open => {
                let files = file_dialog(frame)
                    .set_title("Open XML, JSON or ZIP")
                    .pick_files()
                    .unwrap_or_default();
                for path in files {
                    self.open(path);
                }
            }
            Action::OpenEmbedded { tab, node } => self.open_embedded(tab, node),
            Action::Save => self.save(false, frame),
            Action::SaveAs => self.save(true, frame),
            Action::CloseActive => self.close_tab(self.tabs[index].id),
            Action::Close(id) => self.close_tab(id),
            Action::Activate(index) => {
                self.active = index.min(self.tabs.len() - 1);
                self.focus_editor = true;
            }
            Action::Quit => ctx.send_viewport_cmd(ViewportCommand::Close),
            Action::Search => self.toggle_search(false),
            Action::Replace => self.toggle_search(true),
            Action::HideSearch => {
                self.search_visible = false;
                self.replace_visible = false;
                self.focus_editor = true;
            }
            Action::ReplaceNext => self.replace(false),
            Action::ReplaceAll => self.replace(true),
            Action::Next => self.find(false),
            Action::Previous => self.find(true),
            Action::Select => self.select_at_cursor(false),
            Action::Reveal => self.select_at_cursor(true),
            Action::Format => {
                let tab = &mut self.tabs[index];
                let text: Arc<str> = tab.editor.text().into();
                let kind = tab.kind.unwrap_or_else(|| Kind::detect(&text));
                tab.set_kind(kind);
                tab.status = "Formatting in the background…".into();
                self.submit(index, Task::Format(text, kind));
            }
            Action::Decode => {
                let tab = &mut self.tabs[index];
                let selection = tab.editor.selection();
                let source = tab.editor.text();
                let (text, start, end) = if selection.is_empty() {
                    let end = tab.editor.len_chars();
                    (source.clone(), 0, end)
                } else {
                    (tab.editor.selected_text(), selection.start, selection.end)
                };
                if text.is_empty() {
                    tab.status = "No text to decode".into();
                } else {
                    self.submit(index, Task::Decode(text, source, start, end, false));
                }
            }
            Action::Undo => {
                self.tabs[index].editor.undo();
            }
            Action::Redo => {
                self.tabs[index].editor.redo();
            }
            Action::SelectAll => self.tabs[index].editor.select_all(),
            Action::ParseXml => self.parse(index, Some(Kind::Xml)),
            Action::ParseJson => self.parse(index, Some(Kind::Json)),
            Action::ExpandAll => self.tabs[index].tree.expand_all(),
            Action::CollapseAll => self.tabs[index].tree.collapse_all(),
            Action::CheckForUpdates => self.start_update_check(true),
        }
    }

    fn shortcuts(&self, ctx: &egui::Context) -> Vec<Action> {
        if self.dialog.is_some() {
            return vec![];
        }
        let command = Modifiers::COMMAND;
        let command_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        // More specific shortcuts first: Ctrl+S also matches Ctrl+Shift+S logically.
        let mut table = vec![
            (command_shift, Key::S, Action::SaveAs),
            (command_shift, Key::N, Action::NewWindow),
            (command, Key::S, Action::Save),
            (command, Key::T, Action::New),
            (command, Key::O, Action::Open),
            (command, Key::W, Action::CloseActive),
            (command, Key::Q, Action::Quit),
            (command, Key::E, Action::Select),
            (command, Key::L, Action::Reveal),
            (command, Key::I, Action::Format),
            // Cmd+H hides the app on macOS, so Cmd+Option+F also opens replace.
            (command | Modifiers::ALT, Key::F, Action::Replace),
            (command, Key::H, Action::Replace),
            (command, Key::F, Action::Search),
            (Modifiers::SHIFT, Key::F3, Action::Previous),
            (Modifiers::NONE, Key::F3, Action::Next),
        ];
        let typing_in_search = ctx.memory(|memory| {
            memory.has_focus(Id::unique(SEARCH_ID)) || memory.has_focus(Id::unique(REPLACE_ID))
        });
        if !typing_in_search {
            table.extend([
                (command_shift, Key::Z, Action::Redo),
                (command, Key::Z, Action::Undo),
                (command, Key::Y, Action::Redo),
                (command, Key::A, Action::SelectAll),
            ]);
        }
        if self.search_visible {
            table.push((Modifiers::NONE, Key::Escape, Action::HideSearch));
        }
        ctx.input_mut(|input| {
            table
                .into_iter()
                .filter(|(modifiers, key, _)| {
                    input.consume_shortcut(&KeyboardShortcut::new(*modifiers, *key))
                })
                .map(|(_, _, action)| action)
                .collect()
        })
    }

    fn show_dialog(&mut self, ctx: &egui::Context, frame: Option<&eframe::Frame>) {
        if matches!(self.dialog, Some(Dialog::Zip(_))) {
            self.show_zip_dialog(ctx);
            return;
        }
        let dialog = match self.dialog {
            Some(Dialog::CloseTab(id)) => Dialog::CloseTab(id),
            Some(Dialog::Quit) => Dialog::Quit,
            _ => return,
        };
        let (heading, detail, buttons): (String, &str, &[&str]) = match dialog {
            Dialog::CloseTab(id) => match self.index(id) {
                Some(index) => (
                    format!("Save changes to {}?", self.tabs[index].title()),
                    "",
                    &["Cancel", "Discard", "Save"],
                ),
                None => {
                    self.dialog = None;
                    return;
                }
            },
            Dialog::Quit => (
                "There are unsaved tabs or a save in progress".into(),
                "Save them, discard the changes, or cancel to keep working.",
                &["Cancel", "Discard and quit", "Save all and quit"],
            ),
            Dialog::Zip(_) => return,
        };
        let mut choice = None;
        let response = egui::Modal::new(Id::unique("confirm")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading(heading);
            if !detail.is_empty() {
                ui.label(detail);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for (index, label) in buttons.iter().enumerate() {
                    if ui.button(*label).clicked() {
                        choice = Some(index);
                    }
                }
            });
        });
        if choice.is_none() && response.should_close() {
            choice = Some(0);
        }
        let Some(choice) = choice else {
            return;
        };
        self.dialog = None;
        match (dialog, choice) {
            (Dialog::CloseTab(id), 1) => {
                if let Some(index) = self.index(id) {
                    self.remove_tab(index);
                }
            }
            (Dialog::CloseTab(id), 2) => {
                if let Some(index) = self.index(id) {
                    self.tabs[index].close_after_save = true;
                    match self.tabs[index].path.clone() {
                        Some(path) => self.save_to(index, path),
                        None => self.save_with_dialog(index, frame),
                    }
                }
            }
            (Dialog::Quit, 1) => {
                // Do not terminate a writer halfway through a save.
                if self.tabs.iter().any(|tab| tab.saving) {
                    self.tabs[self.active].status =
                        "Wait for the save to finish before quitting".into();
                } else {
                    self.closing = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
            (Dialog::Quit, 2) => self.save_all_and_quit(frame),
            _ => {}
        }
    }

    /// Saves every modified tab (asking for a path for untitled ones); quits once all saves finish.
    fn save_all_and_quit(&mut self, frame: Option<&eframe::Frame>) {
        for index in 0..self.tabs.len() {
            let tab = &self.tabs[index];
            if !tab.dirty || tab.saving {
                continue;
            }
            let path = match tab.path.clone() {
                Some(path) => path,
                None => {
                    self.active = index;
                    let chosen = file_dialog(frame)
                        .set_title("Save as")
                        .set_file_name(tab.suggested_file_name())
                        .save_file();
                    let Some(path) = chosen else {
                        self.tabs[index].status = "Quit cancelled".into();
                        return;
                    };
                    path
                }
            };
            self.save_to(index, path);
            if !self.tabs[index].saving {
                return;
            }
        }
        self.quit_after_save = true;
    }

    fn quit_when_saved(&mut self, ctx: &egui::Context) {
        if !self.quit_after_save || self.tabs.iter().any(|tab| tab.saving) {
            return;
        }
        self.quit_after_save = false;
        if let Some(index) = self.tabs.iter().position(|tab| tab.dirty) {
            // A failed save keeps its error in the status bar; stay open so nothing is lost.
            self.active = index;
        } else {
            self.closing = true;
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }

    fn show(&mut self, ui: &mut egui::Ui, frame: Option<&eframe::Frame>) {
        let ctx = ui.ctx().clone();
        if self.applied_theme != Some(self.theme) {
            self.theme.apply(&ctx);
            #[cfg(target_os = "windows")]
            if let Some(frame) = frame {
                set_native_titlebar(frame, self.theme.palette());
            }
            self.applied_theme = Some(self.theme);
            // This frame's Ui was created with the previous style.
            ctx.request_repaint();
        }
        let palette = self.theme.palette();
        self.poll();
        self.quit_when_saved(&ctx);
        self.poll_update_check();
        self.receive_open_requests(&ctx);
        let dropped: Vec<PathBuf> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .filter(|path| !path.as_os_str().is_empty())
                .collect()
        });
        for path in dropped {
            self.open(path);
        }
        let mut actions = self.shortcuts(&ctx);

        egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    for (label, action) in [
                        ("New tab", Action::New),
                        ("New window", Action::NewWindow),
                        ("Open…", Action::Open),
                        ("Save", Action::Save),
                        ("Save as…", Action::SaveAs),
                        ("Close tab", Action::CloseActive),
                        ("Quit", Action::Quit),
                    ] {
                        if ui.button(label).clicked() {
                            actions.push(action);
                        }
                    }
                });
                ui.menu_button("Edit", |ui| {
                    for (label, action) in [
                        ("Select element", Action::Select),
                        ("Decode selection", Action::Decode),
                        ("Format document", Action::Format),
                        ("Find", Action::Search),
                        ("Replace", Action::Replace),
                        ("Undo", Action::Undo),
                        ("Redo", Action::Redo),
                    ] {
                        if ui.button(label).clicked() {
                            actions.push(action);
                        }
                    }
                });
                ui.menu_button("Options", |ui| {
                    let response = ui
                        .checkbox(
                            &mut self.open_files_in_new_window,
                            "Open files in new windows",
                        )
                        .on_hover_text(
                            "Files opened from the file explorer or the command line \
                             start their own window instead of a tab",
                        );
                    if response.changed() && self.persist_settings {
                        let value = self.open_files_in_new_window;
                        settings::update(|settings| settings.open_files_in_new_window = value);
                    }
                    let response = ui
                        .checkbox(&mut self.search_case_sensitive, "Case-sensitive search")
                        .on_hover_text("Find and replace distinguish uppercase from lowercase");
                    if response.changed() && self.persist_settings {
                        let value = self.search_case_sensitive;
                        settings::update(|settings| settings.search_case_sensitive = value);
                    }
                    let response = ui
                        .checkbox(&mut self.decode_on_open, "Decode files on open")
                        .on_hover_text(
                            "Opened files are decoded and structured as with Decode; \
                             Ctrl+Z restores the original text",
                        );
                    if response.changed() && self.persist_settings {
                        let value = self.decode_on_open;
                        settings::update(|settings| settings.decode_on_open = value);
                    }
                    ui.menu_button("Theme", |ui| {
                        for theme in Theme::ALL {
                            if ui.radio(self.theme == theme, theme.name()).clicked() {
                                self.theme = theme;
                                if self.persist_settings {
                                    settings::update(|settings| settings.theme = theme.id().into());
                                }
                            }
                        }
                    });
                    let response = ui
                        .checkbox(&mut self.check_for_updates, "Check for updates at startup")
                        .on_hover_text("At most once a day; asks GitHub for the latest release");
                    if response.changed() && self.persist_settings {
                        let value = self.check_for_updates;
                        settings::update(|settings| settings.check_for_updates = value);
                    }
                });
                ui.menu_button("Help", |ui| {
                    ui.label(format!("{APP_TITLE} {CURRENT_VERSION}"));
                    if ui.button("Check for updates").clicked() {
                        actions.push(Action::CheckForUpdates);
                    }
                    if ui.button("Releases page").clicked() {
                        ui.ctx()
                            .open_url(egui::OpenUrl::new_tab(crate::update::RELEASES_PAGE));
                    }
                });
            });
            egui::ScrollArea::horizontal()
                .id_salt("tabs")
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for (index, tab) in self.tabs.iter().enumerate() {
                            let label =
                                format!("{}{}", tab.title(), if tab.dirty { " *" } else { "" });
                            let response = ui.selectable_label(index == self.active, label);
                            if response.clicked() {
                                actions.push(Action::Activate(index));
                            }
                            response.context_menu(|ui| {
                                let movable = tab.path.is_some() && !tab.dirty && !tab.saving;
                                if ui
                                    .add_enabled(movable, egui::Button::new("Move to new window"))
                                    .on_disabled_hover_text("Save the tab first")
                                    .clicked()
                                {
                                    actions.push(Action::MoveToNewWindow(tab.id));
                                }
                                if ui.button("Close tab").clicked() {
                                    actions.push(Action::Close(tab.id));
                                }
                            });
                            if ui.small_button("×").on_hover_text("Close tab").clicked() {
                                actions.push(Action::Close(tab.id));
                            }
                            ui.separator();
                        }
                    });
                });
        });

        if let Some(release) = self.update_available.clone()
            && !self.update_dismissed
        {
            egui::Panel::top("update").resizable(false).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "A new version {} is available (you have {CURRENT_VERSION}).",
                        release.version
                    ));
                    if ui
                        .button("Download…")
                        .on_hover_text("Opens the release page with the downloads and what's new")
                        .clicked()
                    {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(release.url()));
                    }
                    if ui.button("Skip this version").clicked() {
                        self.update_available = None;
                        self.skipped_version = release.version.clone();
                        if self.persist_settings {
                            let version = release.version.clone();
                            settings::update(|settings| settings.skipped_version = version);
                        }
                    }
                    if ui.button("Later").clicked() {
                        self.update_dismissed = true;
                    }
                });
            });
        }

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                for (label, action) in [
                    ("Decode", Action::Decode),
                    ("Parse to JSON", Action::ParseJson),
                    ("Parse to XML", Action::ParseXml),
                    ("New tab", Action::New),
                    ("Collapse all", Action::CollapseAll),
                    ("Expand all", Action::ExpandAll),
                ] {
                    if ui.button(label).clicked() {
                        actions.push(action);
                    }
                }
                let tab = &self.tabs[self.active];
                let (line, column) = tab.editor.line_column();
                ui.label(format!("Ln {}, Col {}", line + 1, column + 1));
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(tab.status.as_str()).truncate());
                });
            });
        });

        let query = if self.search_visible {
            self.search_query.clone()
        } else {
            String::new()
        };
        self.tabs[self.active]
            .editor
            .set_search(&query, self.search_case_sensitive);
        if self.search_visible {
            // Not resizable, so the panel grows when the replace row appears.
            egui::Panel::bottom("search")
                .resizable(false)
                .show(ui, |ui| {
                    // Fixed-height rows: a stretching layout would grow the panel every frame.
                    let row = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
                    let layout = Layout::right_to_left(Align::Center);
                    ui.allocate_ui_with_layout(row, layout, |ui| {
                        for (label, action) in [
                            ("Close", Action::HideSearch),
                            ("Next", Action::Next),
                            ("Previous", Action::Previous),
                        ] {
                            if ui.button(label).clicked() {
                                actions.push(action);
                            }
                        }
                        let editor = &mut self.tabs[self.active].editor;
                        if !self.search_query.is_empty() {
                            ui.label(format!("{} matches", editor.match_count()));
                        }
                        let hint = if self.search_case_sensitive {
                            "Find (case-sensitive)…"
                        } else {
                            "Find (ignoring case)…"
                        };
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut self.search_query)
                                .id(Id::unique(SEARCH_ID))
                                .hint_text(hint)
                                .desired_width(f32::INFINITY),
                        );
                        if std::mem::take(&mut self.focus_search) {
                            response.request_focus();
                        }
                        if response.changed() {
                            editor.set_search(&self.search_query, self.search_case_sensitive);
                        }
                        if response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter))
                        {
                            actions.push(Action::Next);
                            response.request_focus();
                        }
                    });
                    if self.replace_visible {
                        ui.allocate_ui_with_layout(row, layout, |ui| {
                            for (label, action) in [
                                ("Replace all", Action::ReplaceAll),
                                ("Replace", Action::ReplaceNext),
                            ] {
                                if ui.button(label).clicked() {
                                    actions.push(action);
                                }
                            }
                            let response = ui.add(
                                egui::TextEdit::singleline(&mut self.replace_text)
                                    .id(Id::unique(REPLACE_ID))
                                    .hint_text("Replace with…")
                                    .desired_width(f32::INFINITY),
                            );
                            if response.lost_focus()
                                && ui.input(|input| input.key_pressed(Key::Enter))
                            {
                                actions.push(Action::ReplaceNext);
                                response.request_focus();
                            }
                        });
                    }
                });
        }

        let tab = &mut self.tabs[self.active];
        let current = tab.tree_current();
        let pointer_down = ctx.input(|input| input.pointer.any_down());
        let panel = egui::Panel::left("tree")
            .resizable(true)
            .default_size(self.tree_width)
            .min_size(TREE_MIN_WIDTH)
            .frame(egui::Frame::NONE.fill(palette.background))
            .show(ui, |ui| {
                if let Some(error) = &tab.parse_error
                    && tab.editor.len_chars() > 0
                {
                    egui::Frame::NONE.inner_margin(8).show(ui, |ui| {
                        ui.strong("No tree: the text is not valid JSON or XML");
                        ui.weak(error.as_str());
                        ui.label(
                            "For escaped logs use Decode (or Options › Decode files on open); \
                             otherwise try Parse to JSON / Parse to XML.",
                        );
                    });
                    ui.separator();
                }
                let chosen = ui
                    .add_enabled_ui(current, |ui| {
                        tab.tree.ui(ui, Id::unique(("tree", tab.id)), palette)
                    })
                    .inner;
                match chosen {
                    Some(crate::tree::TreeAction::Select(id)) => tab.select_node(id),
                    Some(crate::tree::TreeAction::OpenEmbedded(node)) => {
                        actions.push(Action::OpenEmbedded { tab: tab.id, node });
                    }
                    None => {}
                }
            });
        let width = panel.response.rect.width();
        if (width - self.tree_width).abs() > 0.5 {
            self.tree_width = width;
            // Only user drags are remembered, not clamping by a small window.
            self.tree_resized |= pointer_down;
        }
        if self.tree_resized && !pointer_down {
            self.tree_resized = false;
            if self.persist_settings {
                let width = self.tree_width;
                settings::update(|settings| settings.tree_width = width);
            }
        }
        let focus = self.dialog.is_none() && std::mem::take(&mut self.focus_editor);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(palette.background))
            .show(ui, |ui| {
                tab.editor
                    .ui(ui, Id::unique(("editor", tab.id)), focus, palette);
            });

        if !actions.is_empty() {
            ctx.request_repaint();
        }
        for action in actions {
            self.perform(action, &ctx, frame);
        }
        if let Some(wait) = self.process_changes(Instant::now()) {
            ctx.request_repaint_after(wait);
        }
        if ctx.input(|input| input.viewport().close_requested())
            && !self.closing
            && self.tabs.iter().any(|tab| tab.dirty || tab.saving)
        {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.dialog = Some(Dialog::Quit);
        }
        self.show_dialog(&ctx, frame);
        let title = format!("{APP_TITLE} — {}", self.tabs[self.active].title());
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.show(ui, Some(frame));
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RendererChoice {
    #[default]
    Auto,
    Wgpu,
    WgpuDx12,
    WgpuSoftware,
    Glow,
}

impl RendererChoice {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "wgpu" => Some(Self::Wgpu),
            "dx12" => Some(Self::WgpuDx12),
            "software" | "warp" => Some(Self::WgpuSoftware),
            "gl" | "glow" | "opengl" => Some(Self::Glow),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartupAttempt {
    WgpuDefault,
    WgpuDx12,
    WgpuSoftware,
    Glow,
}

impl StartupAttempt {
    fn id(self) -> &'static str {
        match self {
            Self::WgpuDefault => "wgpu",
            Self::WgpuDx12 => "wgpu-dx12",
            Self::WgpuSoftware => "wgpu-software",
            Self::Glow => "glow",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::WgpuDefault => "wgpu (default)",
            Self::WgpuDx12 => "wgpu (DX12 / WARP fallback)",
            Self::WgpuSoftware => "wgpu (software / WARP)",
            Self::Glow => "OpenGL (glow)",
        }
    }
}

fn startup_plan(
    choice: RendererChoice,
    previous_crash: Option<&str>,
    remembered: Option<&str>,
    is_windows: bool,
) -> Vec<StartupAttempt> {
    match choice {
        RendererChoice::Wgpu => return vec![StartupAttempt::WgpuDefault],
        RendererChoice::WgpuDx12 => return vec![StartupAttempt::WgpuDx12],
        RendererChoice::WgpuSoftware => return vec![StartupAttempt::WgpuSoftware],
        RendererChoice::Glow => return vec![StartupAttempt::Glow],
        RendererChoice::Auto => {}
    }
    let mut plan = if is_windows {
        match previous_crash {
            Some("wgpu-dx12") | Some("wgpu") => vec![
                StartupAttempt::Glow,
                StartupAttempt::WgpuSoftware,
                StartupAttempt::WgpuDx12,
            ],
            Some("glow") => vec![
                StartupAttempt::WgpuSoftware,
                StartupAttempt::WgpuDx12,
                StartupAttempt::Glow,
            ],
            _ => vec![
                StartupAttempt::WgpuDx12,
                StartupAttempt::Glow,
                StartupAttempt::WgpuSoftware,
            ],
        }
    } else {
        match previous_crash {
            Some("wgpu") => vec![StartupAttempt::Glow, StartupAttempt::WgpuDefault],
            _ => vec![StartupAttempt::WgpuDefault, StartupAttempt::Glow],
        }
    };
    // A native driver crash cannot be retried in-process, so keep starting with what worked.
    if previous_crash.is_none()
        && let Some(index) = plan
            .iter()
            .position(|attempt| Some(attempt.id()) == remembered)
    {
        let attempt = plan.remove(index);
        plan.insert(0, attempt);
    }
    plan
}

fn select_wgpu_adapter(
    adapters: &[eframe::wgpu::Adapter],
    surface: Option<&eframe::wgpu::Surface<'_>>,
    software_only: bool,
) -> Result<eframe::wgpu::Adapter, String> {
    use eframe::wgpu::DeviceType;

    let mut best: Option<(u8, &eframe::wgpu::Adapter)> = None;
    for adapter in adapters {
        if let Some(surface) = surface
            && !adapter.is_surface_supported(surface)
        {
            continue;
        }
        let info = adapter.get_info();
        if software_only && info.device_type != DeviceType::Cpu {
            continue;
        }
        let rank = match info.device_type {
            DeviceType::IntegratedGpu => 0,
            DeviceType::DiscreteGpu => 1,
            DeviceType::VirtualGpu => 2,
            DeviceType::Other => 3,
            DeviceType::Cpu => 4,
        };
        if best.is_none_or(|(best_rank, _)| rank < best_rank) {
            best = Some((rank, adapter));
        }
    }
    if let Some((_, adapter)) = best {
        let info = adapter.get_info();
        crate::diagnostics::record(&format!(
            "Selected graphics adapter: {} ({:?}, {:?})",
            info.name, info.backend, info.device_type
        ));
        Ok(adapter.clone())
    } else if software_only {
        Err("No compatible software graphics adapter found".into())
    } else {
        Err("No compatible graphics adapter found".into())
    }
}

fn native_options(icon: eframe::egui::IconData, attempt: StartupAttempt) -> eframe::NativeOptions {
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_TITLE)
            .with_app_id(APP_ID)
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([480.0, 320.0])
            .with_icon(icon)
            .with_drag_and_drop(true),
        ..Default::default()
    };
    match attempt {
        StartupAttempt::Glow => {
            options.renderer = eframe::Renderer::Glow;
        }
        StartupAttempt::WgpuDefault => {
            options.renderer = eframe::Renderer::Wgpu;
            if let eframe::egui_wgpu::WgpuSetup::CreateNew(ref mut setup) =
                options.wgpu_options.wgpu_setup
            {
                setup.power_preference = eframe::wgpu::PowerPreference::from_env()
                    .unwrap_or(eframe::wgpu::PowerPreference::LowPower);
                setup.native_adapter_selector = Some(Arc::new(|adapters, surface| {
                    select_wgpu_adapter(adapters, surface, false)
                }));
            }
        }
        StartupAttempt::WgpuDx12 => {
            options.renderer = eframe::Renderer::Wgpu;
            if let eframe::egui_wgpu::WgpuSetup::CreateNew(ref mut setup) =
                options.wgpu_options.wgpu_setup
            {
                // On Windows, avoid probing Vulkan/WGL by default because faulty
                // Vulkan ICDs (e.g. older Intel/hybrid drivers) can cause native
                // access violations during adapter enumeration.
                setup.instance_descriptor.backends =
                    eframe::wgpu::Backends::from_env().unwrap_or(eframe::wgpu::Backends::DX12);
                setup.power_preference = eframe::wgpu::PowerPreference::from_env()
                    .unwrap_or(eframe::wgpu::PowerPreference::LowPower);
                setup.native_adapter_selector = Some(Arc::new(|adapters, surface| {
                    select_wgpu_adapter(adapters, surface, false)
                }));
            }
        }
        StartupAttempt::WgpuSoftware => {
            options.renderer = eframe::Renderer::Wgpu;
            if let eframe::egui_wgpu::WgpuSetup::CreateNew(ref mut setup) =
                options.wgpu_options.wgpu_setup
            {
                setup.instance_descriptor.backends =
                    eframe::wgpu::Backends::from_env().unwrap_or(eframe::wgpu::Backends::DX12);
                setup.native_adapter_selector = Some(Arc::new(|adapters, surface| {
                    select_wgpu_adapter(adapters, surface, true)
                }));
            }
        }
    }
    options
}

pub fn run(
    files: Vec<PathBuf>,
    requests: mpsc::Receiver<Vec<PathBuf>>,
    choice: RendererChoice,
) -> eframe::Result {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../resources/visor-mascot.png"))
        .expect("Embedded viewer icon must be a valid PNG");
    let previous_crash = crate::diagnostics::previous_graphics_init_failure();
    if let Some(failed) = &previous_crash {
        crate::diagnostics::record(&format!(
            "Previous launch did not finish graphics initialization ({failed}); prioritizing fallback renderer"
        ));
    }
    let remembered = (choice == RendererChoice::Auto)
        .then(crate::diagnostics::remembered_renderer)
        .flatten();
    if let Some(renderer) = &remembered {
        crate::diagnostics::record(&format!("Last working graphics backend: {renderer}"));
    }
    let plan = startup_plan(
        choice,
        previous_crash.as_deref(),
        remembered.as_deref(),
        cfg!(target_os = "windows"),
    );
    let requests = Arc::new(Mutex::new(Some(requests)));
    let settings = settings::load();
    let mut last_error: Option<eframe::Error> = None;

    for (index, attempt) in plan.iter().copied().enumerate() {
        crate::diagnostics::record(&format!(
            "Trying graphics backend {}/{}: {}",
            index + 1,
            plan.len(),
            attempt.description()
        ));
        crate::diagnostics::mark_graphics_init_started(attempt.id());

        let options = native_options(icon.clone(), attempt);
        let files_for_attempt = files.clone();
        let requests_for_attempt = requests.clone();
        let settings_for_attempt = settings.clone();
        let initialized = Arc::new(AtomicBool::new(false));
        let initialized_flag = initialized.clone();

        let result = eframe::run_native(
            APP_TITLE,
            options,
            Box::new(move |creation| {
                crate::diagnostics::mark_graphics_init_succeeded();
                if choice == RendererChoice::Auto {
                    crate::diagnostics::remember_renderer(attempt.id());
                }
                initialized_flag.store(true, Ordering::Relaxed);
                crate::diagnostics::record(&format!(
                    "Window and graphics context created ({}); initializing editor",
                    attempt.description()
                ));
                let mut app = App::new(Some(creation.egui_ctx.clone()));
                let update_due = crate::update::should_check(
                    settings_for_attempt.check_for_updates,
                    settings_for_attempt.last_update_check,
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |elapsed| elapsed.as_secs()),
                );
                app.apply_settings(settings_for_attempt);
                if update_due {
                    app.start_update_check(false);
                }
                // Applied before the first frame so the window does not flash the default theme.
                app.theme.apply(&creation.egui_ctx);
                #[cfg(target_os = "windows")]
                set_native_titlebar(creation, app.theme.palette());
                app.applied_theme = Some(app.theme);
                app.open_requests = requests_for_attempt
                    .lock()
                    .ok()
                    .and_then(|mut guard| guard.take());
                for path in files_for_attempt {
                    app.open(path);
                }
                crate::diagnostics::record("Editor initialized");
                Ok(Box::new(app))
            }),
        );

        match result {
            Ok(()) => {
                crate::diagnostics::mark_graphics_init_succeeded();
                return Ok(());
            }
            Err(error) => {
                crate::diagnostics::record(&format!(
                    "Graphics backend {} failed: {error}",
                    attempt.description()
                ));
                if initialized.load(Ordering::Relaxed) {
                    return Err(error);
                }
                crate::diagnostics::mark_graphics_init_succeeded();
                last_error = Some(error);
            }
        }
    }

    Err(last_error
        .unwrap_or_else(|| eframe::Error::AppCreation(Box::from("No graphics backend succeeded"))))
}
