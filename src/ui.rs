use crate::{
    document::{self, Document, Kind},
    editor::{self, Editor},
    tree::Tree,
    tree_style,
};
use eframe::egui::{
    self, Align, Id, Key, KeyboardShortcut, Layout, Modifiers, ThemePreference, ViewportCommand,
};
use std::{
    ffi::OsString,
    fs, io,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[cfg(test)]
#[path = "ui_tests.rs"]
mod tests;

const APP_TITLE: &str = "XML/JSON Viewer Rust";
pub const APP_ID: &str = "io.github.xmljsonviewer.Viewer";
const DEBOUNCE: Duration = Duration::from_millis(350);
const SEARCH_ID: &str = "search-input";

enum Task {
    Parse(Arc<str>, Kind),
    Open(PathBuf),
    Format(Arc<str>, Kind),
    Decode(String, usize, usize),
    Save(PathBuf, String),
}

struct Job {
    tab: u64,
    revision: u64,
    current: Arc<AtomicU64>,
    task: Task,
}

enum Output {
    Parsed(Result<Arc<Document>, String>),
    Opened(
        PathBuf,
        Kind,
        Result<Arc<Document>, String>,
        Option<Arc<str>>,
    ),
    Formatted(Result<String, String>),
    Decoded(Result<Option<String>, String>, usize, usize),
    Saved(PathBuf, Result<(), String>),
}

struct Reply {
    tab: u64,
    revision: u64,
    output: Output,
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
                Task::Open(path) => match fs::read_to_string(&path) {
                    Ok(text) => {
                        let kind = if path
                            .extension()
                            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
                        {
                            Kind::Json
                        } else {
                            Kind::detect(&text)
                        };
                        let text: Arc<str> = text.into();
                        let parsed = document::parse(text.clone(), kind, cancelled).map(Arc::new);
                        Output::Opened(path, kind, parsed, Some(text))
                    }
                    Err(error) => Output::Opened(
                        path,
                        Kind::Xml,
                        Err(format!("Could not open file: {error}")),
                        None,
                    ),
                },
                Task::Format(text, kind) => Output::Formatted(document::format(&text, kind)),
                Task::Decode(text, start, end) => {
                    Output::Decoded(document::decode(&text), start, end)
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
    dirty: bool,
    saving: bool,
    close_after_save: bool,
    status: String,
    parse_count: usize,
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
            dirty: false,
            saving: false,
            close_after_save: false,
            status: "Ready".into(),
            parse_count: 0,
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
            .unwrap_or_else(|| "Untitled".into())
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
            }
            Err(error) => self.status = error,
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
    Open,
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
    Next,
    Previous,
    HideSearch,
    Undo,
    Redo,
    SelectAll,
    ParseXml,
    ParseJson,
    Decode,
}

#[derive(Clone, Copy)]
enum Dialog {
    CloseTab(u64),
    Quit,
}

pub struct App {
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    jobs: mpsc::Sender<Job>,
    replies: mpsc::Receiver<Reply>,
    search_visible: bool,
    search_query: String,
    focus_search: bool,
    focus_editor: bool,
    dialog: Option<Dialog>,
    closing: bool,
    title: String,
}

fn file_dialog(frame: Option<&eframe::Frame>) -> rfd::FileDialog {
    let dialog = rfd::FileDialog::new()
        .add_filter("XML and JSON", &["xml", "json"])
        .add_filter("All files", &["*"]);
    match frame {
        Some(frame) => dialog.set_parent(frame),
        None => dialog,
    }
}

impl App {
    fn new(ctx: Option<egui::Context>) -> Self {
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
            search_visible: false,
            search_query: String::new(),
            focus_search: false,
            focus_editor: true,
            dialog: None,
            closing: false,
            title: String::new(),
        };
        app.new_tab();
        app
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
        let Some(index) = self.index(reply.tab) else {
            return;
        };
        let tab = &mut self.tabs[index];
        if reply.revision != tab.revision() && !matches!(reply.output, Output::Saved(..)) {
            return;
        }
        match reply.output {
            Output::Parsed(parsed) => tab.install_document(reply.revision, parsed),
            Output::Opened(path, kind, parsed, content) => {
                tab.editor.editable = true;
                if let Some(content) = content {
                    tab.set_kind(kind);
                    tab.editor.set_text(&content);
                    tab.seen_version = tab.editor.version();
                    tab.path = Some(path);
                    tab.dirty = false;
                    tab.install_document(reply.revision, parsed);
                } else if let Err(error) = parsed {
                    tab.status = error;
                }
            }
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
            Output::Decoded(Ok(Some(text)), start, end) => {
                tab.editor.replace(start..end, &text);
                tab.note_changes(Instant::now());
                tab.status = "Selection decoded".into();
            }
            Output::Decoded(Ok(None), ..) => tab.status = "No escaped content to decode".into(),
            Output::Decoded(Err(error), ..) => tab.status = format!("Decode error: {error}"),
            Output::Saved(path, result) => {
                tab.saving = false;
                match result {
                    Ok(()) => {
                        tab.path = Some(path);
                        let unchanged = reply.revision == tab.revision();
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
        tab.editor.set_search(&self.search_query);
        if !tab.editor.find(backwards) {
            tab.status = "No matches found".into();
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
            .set_file_name(tab.title())
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
            Action::Open => {
                let files = file_dialog(frame)
                    .set_title("Open XML/JSON")
                    .pick_files()
                    .unwrap_or_default();
                for path in files {
                    self.open(path);
                }
            }
            Action::Save => self.save(false, frame),
            Action::SaveAs => self.save(true, frame),
            Action::CloseActive => self.close_tab(self.tabs[index].id),
            Action::Close(id) => self.close_tab(id),
            Action::Activate(index) => {
                self.active = index.min(self.tabs.len() - 1);
                self.focus_editor = true;
            }
            Action::Quit => ctx.send_viewport_cmd(ViewportCommand::Close),
            Action::Search if !self.search_visible => {
                self.search_visible = true;
                self.focus_search = true;
            }
            Action::Search | Action::HideSearch => {
                self.search_visible = false;
                self.focus_editor = true;
            }
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
                if selection.is_empty() {
                    tab.status = "Select text first (Ctrl+E)".into();
                } else {
                    let text = tab.editor.selected_text();
                    self.submit(index, Task::Decode(text, selection.start, selection.end));
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
            (command, Key::S, Action::Save),
            (command, Key::T, Action::New),
            (command, Key::O, Action::Open),
            (command, Key::W, Action::CloseActive),
            (command, Key::Q, Action::Quit),
            (command, Key::E, Action::Select),
            (command, Key::L, Action::Reveal),
            (command, Key::I, Action::Format),
            (command, Key::F, Action::Search),
            (Modifiers::SHIFT, Key::F3, Action::Previous),
            (Modifiers::NONE, Key::F3, Action::Next),
        ];
        if !ctx.memory(|memory| memory.has_focus(Id::unique(SEARCH_ID))) {
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
        let Some(dialog) = self.dialog else {
            return;
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
                "Cancel to save your changes before quitting.",
                &["Cancel", "Discard and quit"],
            ),
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
            _ => {}
        }
    }

    fn show(&mut self, ui: &mut egui::Ui, frame: Option<&eframe::Frame>) {
        let ctx = ui.ctx().clone();
        self.poll();
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
                        ("Undo", Action::Undo),
                        ("Redo", Action::Redo),
                    ] {
                        if ui.button(label).clicked() {
                            actions.push(action);
                        }
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
                            if ui.selectable_label(index == self.active, label).clicked() {
                                actions.push(Action::Activate(index));
                            }
                            if ui.small_button("×").on_hover_text("Close tab").clicked() {
                                actions.push(Action::Close(tab.id));
                            }
                            ui.separator();
                        }
                    });
                });
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                for (label, action) in [
                    ("Decode", Action::Decode),
                    ("Parse to JSON", Action::ParseJson),
                    ("Parse to XML", Action::ParseXml),
                    ("New tab", Action::New),
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
        self.tabs[self.active].editor.set_search(&query);
        if self.search_visible {
            egui::Panel::bottom("search").show(ui, |ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
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
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut self.search_query)
                            .id(Id::unique(SEARCH_ID))
                            .hint_text("Find (case-sensitive)…")
                            .desired_width(f32::INFINITY),
                    );
                    if std::mem::take(&mut self.focus_search) {
                        response.request_focus();
                    }
                    if response.changed() {
                        editor.set_search(&self.search_query);
                    }
                    if response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)) {
                        actions.push(Action::Next);
                        response.request_focus();
                    }
                });
            });
        }

        let tab = &mut self.tabs[self.active];
        let current = tab.tree_current();
        egui::Panel::left("tree")
            .resizable(true)
            .default_size(280.0)
            .min_size(160.0)
            .frame(egui::Frame::NONE.fill(tree_style::BACKGROUND))
            .show(ui, |ui| {
                let chosen = ui
                    .add_enabled_ui(current, |ui| tab.tree.ui(ui, Id::unique(("tree", tab.id))))
                    .inner;
                if let Some(id) = chosen {
                    tab.select_node(id);
                }
            });
        let focus = self.dialog.is_none() && std::mem::take(&mut self.focus_editor);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(editor::BACKGROUND))
            .show(ui, |ui| {
                tab.editor.ui(ui, Id::unique(("editor", tab.id)), focus);
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

pub fn run(files: Vec<PathBuf>) -> eframe::Result {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../resources/visor-mascot.png"))
        .expect("Embedded viewer icon must be a valid PNG");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_TITLE)
            .with_app_id(APP_ID)
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([480.0, 320.0])
            .with_icon(icon)
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        APP_TITLE,
        options,
        Box::new(|creation| {
            creation.egui_ctx.set_theme(ThemePreference::Dark);
            let mut app = App::new(Some(creation.egui_ctx.clone()));
            for path in files {
                app.open(path);
            }
            Ok(Box::new(app))
        }),
    )
}
