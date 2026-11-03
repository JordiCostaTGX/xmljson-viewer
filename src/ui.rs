use crate::{
    document::{self, Document, Kind},
    syntax::{self, Highlight, Style},
    tree, tree_style,
};
use gtk::{gdk, gio, glib, prelude::*};
use sourceview::prelude::*;
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::{Rc, Weak},
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

enum Task {
    Parse(Arc<str>, Kind),
    Open(PathBuf),
    Format(Arc<str>, Kind),
    Decode(String, i32, i32),
    Save(PathBuf, String),
}

struct Job {
    tab: u64,
    revision: u64,
    current: Arc<AtomicU64>,
    task: Task,
}

enum Output {
    Parsed(Result<Arc<Document>, String>, Option<Highlight>),
    Opened(
        PathBuf,
        Kind,
        Result<Arc<Document>, String>,
        Option<String>,
        Option<Highlight>,
        bool,
    ),
    Formatted(Result<String, String>),
    Decoded(Result<Option<String>, String>, i32, i32),
    Saved(PathBuf, Result<(), String>),
}

struct Reply {
    tab: u64,
    revision: u64,
    output: Output,
}

fn worker() -> (mpsc::Sender<Job>, mpsc::Receiver<Reply>) {
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
                    let colors = (kind == Kind::Json).then(|| syntax::analyze(&text, cancelled));
                    Output::Parsed(document::parse(text, kind, cancelled).map(Arc::new), colors)
                }
                Task::Open(path) => match std::fs::read_to_string(&path) {
                    Ok(mut text) => {
                        let kind = if path
                            .extension()
                            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
                        {
                            Kind::Json
                        } else {
                            Kind::detect(&text)
                        };
                        let mut adapted = false;
                        if kind == Kind::Json && text.lines().any(|line| line.len() > 16_384) {
                            if let Ok(formatted) = document::format(&text, Kind::Json) {
                                text = formatted;
                                adapted = true;
                            }
                        }
                        let colors =
                            (kind == Kind::Json).then(|| syntax::analyze(&text, cancelled));
                        let parsed = document::parse(Arc::from(text.as_str()), kind, cancelled)
                            .map(Arc::new);
                        Output::Opened(path, kind, parsed, Some(text), colors, adapted)
                    }
                    Err(error) => Output::Opened(
                        path,
                        Kind::Xml,
                        Err(format!("Could not open file: {error}")),
                        None,
                        None,
                        false,
                    ),
                },
                Task::Format(text, kind) => Output::Formatted(document::format(&text, kind)),
                Task::Decode(text, start, end) => {
                    Output::Decoded(document::decode(&text), start, end)
                }
                Task::Save(path, text) => {
                    let result = gio::File::for_path(&path)
                        .replace_contents(
                            text.as_bytes(),
                            None,
                            false,
                            gio::FileCreateFlags::NONE,
                            None::<&gio::Cancellable>,
                        )
                        .map(|_| ())
                        .map_err(|e| format!("Could not save file: {e}"));
                    Output::Saved(path, result)
                }
            };
            if replies
                .send(Reply {
                    tab: job.tab,
                    revision: job.revision,
                    output,
                })
                .is_err()
            {
                break;
            }
        }
    });
    (send, receive)
}

struct Tab {
    id: u64,
    page: gtk::Paned,
    title: gtk::Label,
    buffer: sourceview::Buffer,
    editor: sourceview::View,
    selection: gtk::SingleSelection,
    list: gtk::ListView,
    model: RefCell<Option<gtk::TreeListModel>>,
    document: RefCell<Option<Arc<Document>>>,
    document_revision: Cell<u64>,
    revision: Arc<AtomicU64>,
    debounce: RefCell<Option<glib::SourceId>>,
    kind: Cell<Option<Kind>>,
    path: RefCell<Option<PathBuf>>,
    dirty: Cell<bool>,
    suppress: Cell<bool>,
    saving: Cell<bool>,
    close_after_save: Cell<bool>,
    status: RefCell<String>,
    search: sourceview::SearchContext,
    bound_rows: Cell<usize>,
    parse_count: Cell<usize>,
    colors: [gtk::TextTag; 4],
    color_job: RefCell<Option<glib::SourceId>>,
    color_revision: Cell<u64>,
    syntax: RefCell<Option<(u64, Highlight)>>,
    color_coverage: Cell<Option<(u64, i32, i32)>>,
    hidden: gtk::TextTag,
    fold_focus: Cell<Option<i32>>,
    fold_notice: gtk::Label,
}

impl Tab {
    fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    fn text(&self) -> String {
        self.buffer
            .text(&self.buffer.start_iter(), &self.buffer.end_iter(), true)
            .to_string()
    }

    fn title(&self) -> String {
        self.path
            .borrow()
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".into())
    }

    fn update_title(&self) {
        self.title.set_text(&format!(
            "{}{}",
            self.title(),
            if self.dirty.get() { " *" } else { "" }
        ));
    }

    fn set_kind(&self, kind: Kind) {
        self.kind.set(Some(kind));
        if kind == Kind::Json {
            // GtkSourceView disables its regex engine after 2 s on one long line.
            // JSON uses a linear Rust lexer instead, including incomplete edits.
            self.buffer.set_language(None);
        } else {
            self.buffer.remove_tag(
                &self.hidden,
                &self.buffer.start_iter(),
                &self.buffer.end_iter(),
            );
            self.fold_notice.set_visible(false);
            for color in &self.colors {
                self.buffer
                    .remove_tag(color, &self.buffer.start_iter(), &self.buffer.end_iter());
            }
            self.buffer.set_language(
                sourceview::LanguageManager::default()
                    .language(kind.language())
                    .as_ref(),
            );
        }
    }

    fn select_range(&self, start: i32, end: i32) {
        let mut first = self.buffer.iter_at_offset(start);
        let last = self.buffer.iter_at_offset(end);
        self.buffer.select_range(&first, &last);
        self.editor
            .scroll_to_iter(&mut first, 0.15, false, 0.0, 0.0);
    }
}

struct App {
    application: gtk::Application,
    window: gtk::ApplicationWindow,
    notebook: gtk::Notebook,
    tabs: RefCell<Vec<Rc<Tab>>>,
    next_id: Cell<u64>,
    status: gtk::Label,
    position: gtk::Label,
    search_bar: gtk::Box,
    search_input: gtk::SearchEntry,
    match_label: gtk::Label,
    jobs: mpsc::Sender<Job>,
    closing: Cell<bool>,
}

impl App {
    fn new(application: &gtk::Application) -> Rc<Self> {
        gtk::Settings::default()
            .map(|settings| settings.set_gtk_application_prefer_dark_theme(true));
        let window = gtk::ApplicationWindow::builder()
            .application(application)
            .title("XML/JSON Viewer Rust")
            .default_width(1280)
            .default_height(800)
            .build();
        if let Some(display) = gdk::Display::default() {
            tree_style::install(&display);
            let theme = gtk::IconTheme::for_display(&display);
            theme.add_resource_path("/io/github/xmljsonviewer/Viewer/icons");
            window.set_icon_name(Some("xmljson-viewer-rust"));
        }
        let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let menus = gio::Menu::new();
        let file = gio::Menu::new();
        for (label, action) in [
            ("New tab", "win.new"),
            ("Open…", "win.open"),
            ("Save", "win.save"),
            ("Save as…", "win.save-as"),
            ("Close tab", "win.close-tab"),
            ("Quit", "win.quit"),
        ] {
            file.append(Some(label), Some(action));
        }
        let edit = gio::Menu::new();
        for (label, action) in [
            ("Select element", "win.select"),
            ("Decode selection", "win.decode"),
            ("Format document", "win.format"),
            ("Find", "win.search"),
            ("Undo", "win.undo"),
            ("Redo", "win.redo"),
        ] {
            edit.append(Some(label), Some(action));
        }
        menus.append_submenu(Some("File"), &file);
        menus.append_submenu(Some("Edit"), &edit);
        layout.append(&gtk::PopoverMenuBar::from_model(Some(&menus)));
        let notebook = gtk::Notebook::new();
        notebook.set_scrollable(true);
        notebook.set_hexpand(true);
        notebook.set_vexpand(true);
        layout.append(&notebook);
        let search_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        search_bar.set_margin_start(6);
        search_bar.set_margin_end(6);
        let search_input = gtk::SearchEntry::new();
        search_input.set_hexpand(true);
        search_input.set_placeholder_text(Some("Find (case-sensitive)…"));
        search_bar.append(&search_input);
        for (label, action) in [
            ("Previous", "win.previous"),
            ("Next", "win.next"),
            ("Close", "win.hide-search"),
        ] {
            let button = gtk::Button::with_label(label);
            button.set_action_name(Some(action));
            search_bar.append(&button);
        }
        let match_label = gtk::Label::new(None);
        search_bar.append(&match_label);
        search_bar.set_visible(false);
        layout.append(&search_bar);
        let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bottom.set_margin_start(6);
        bottom.set_margin_end(6);
        let status = gtk::Label::new(Some("Ready"));
        status.set_hexpand(true);
        status.set_xalign(0.0);
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);
        bottom.append(&status);
        let position = gtk::Label::new(Some("Ln 1, Col 1"));
        bottom.append(&position);
        for (label, action) in [
            ("New tab", "win.new"),
            ("Parse to XML", "win.parse-xml"),
            ("Parse to JSON", "win.parse-json"),
            ("Decode", "win.decode"),
        ] {
            let button = gtk::Button::with_label(label);
            button.set_action_name(Some(action));
            bottom.append(&button);
        }
        layout.append(&bottom);
        window.set_child(Some(&layout));
        let (jobs, replies) = worker();
        let app = Rc::new(Self {
            application: application.clone(),
            window,
            notebook,
            tabs: RefCell::new(vec![]),
            next_id: Cell::new(0),
            status,
            position,
            search_bar,
            search_input,
            match_label,
            jobs,
            closing: Cell::new(false),
        });
        app.actions();
        app.new_tab();
        let weak = Rc::downgrade(&app);
        glib::timeout_add_local(Duration::from_millis(30), move || {
            let Some(app) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            // Limit UI work per tick so many completions cannot monopolize the event loop.
            for _ in 0..4 {
                match replies.try_recv() {
                    Ok(reply) => app.accept(reply),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        app.status.set_text("Error: the parsing worker has stopped");
                        return glib::ControlFlow::Break;
                    }
                }
            }
            glib::ControlFlow::Continue
        });
        let weak = Rc::downgrade(&app);
        app.notebook.connect_switch_page(move |_, _, _| {
            if let Some(app) = weak.upgrade() {
                let weak = Rc::downgrade(&app);
                glib::idle_add_local_once(move || {
                    if let Some(app) = weak.upgrade() {
                        app.refresh();
                    }
                });
            }
        });
        let weak = Rc::downgrade(&app);
        app.search_input.connect_search_changed(move |entry| {
            if let Some(app) = weak.upgrade() {
                if let Some(tab) = app.active() {
                    let text = entry.text();
                    tab.search.settings().set_search_text(if text.is_empty() {
                        None
                    } else {
                        Some(text.as_str())
                    });
                    app.refresh_search();
                }
            }
        });
        let weak = Rc::downgrade(&app);
        app.search_input.connect_activate(move |_| {
            if let Some(app) = weak.upgrade() {
                app.find(false);
            }
        });
        let weak = Rc::downgrade(&app);
        app.search_input.connect_stop_search(move |_| {
            if let Some(app) = weak.upgrade() {
                app.hide_search();
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.connect_close_request(move |_| {
            let Some(app) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if app.closing.get()
                || !app
                    .tabs
                    .borrow()
                    .iter()
                    .any(|t| t.dirty.get() || t.saving.get())
            {
                return glib::Propagation::Proceed;
            }
            let dialog = gtk::MessageDialog::builder()
                .transient_for(&app.window)
                .modal(true)
                .text("There are unsaved tabs or a save in progress")
                .secondary_text("Cancel to save your changes before quitting.")
                .build();
            dialog.add_button("Cancel", gtk::ResponseType::Cancel);
            dialog.add_button("Discard and quit", gtk::ResponseType::Accept);
            dialog.set_default_response(gtk::ResponseType::Cancel);
            let weak = Rc::downgrade(&app);
            dialog.connect_response(move |dialog, response| {
                dialog.close();
                if response == gtk::ResponseType::Accept {
                    if let Some(app) = weak.upgrade() {
                        // Do not terminate a writer halfway through a save.
                        if app.tabs.borrow().iter().any(|t| t.saving.get()) {
                            app.status
                                .set_text("Wait for the save to finish before quitting");
                        } else {
                            app.closing.set(true);
                            app.window.close();
                        }
                    }
                }
            });
            dialog.present();
            glib::Propagation::Stop
        });
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let weak = Rc::downgrade(&app);
        drop.connect_drop(move |_, value, _, _| {
            let (Some(app), Ok(files)) = (weak.upgrade(), value.get::<gdk::FileList>()) else {
                return false;
            };
            for file in files.files() {
                if let Some(path) = file.path() {
                    app.open(path);
                } else {
                    app.status.set_text("Only local files are supported");
                }
            }
            true
        });
        app.window.add_controller(drop);
        app
    }

    fn active(&self) -> Option<Rc<Tab>> {
        let page = self.notebook.nth_page(self.notebook.current_page())?;
        self.tabs
            .borrow()
            .iter()
            .find(|tab| tab.page == page)
            .cloned()
    }

    fn message(&self, tab: &Tab, message: impl Into<String>) {
        let message = message.into();
        tab.status.replace(message.clone());
        if self.active().is_some_and(|t| t.id == tab.id) {
            self.status.set_text(&message);
        }
    }

    fn new_tab(self: &Rc<Self>) -> Rc<Tab> {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        let buffer = sourceview::Buffer::new(None);
        buffer.set_highlight_syntax(true);
        buffer.set_enable_undo(true);
        let styles = sourceview::StyleSchemeManager::default();
        if let Some(style) = styles
            .scheme("Adwaita-dark")
            .or_else(|| styles.scheme("oblivion"))
        {
            buffer.set_style_scheme(Some(&style));
        }
        let editor = sourceview::View::with_buffer(&buffer);
        editor.set_monospace(true);
        editor.set_show_line_numbers(true);
        editor.set_highlight_current_line(true);
        editor.set_tab_width(2);
        editor.set_insert_spaces_instead_of_tabs(true);
        editor.set_auto_indent(true);
        editor.set_wrap_mode(gtk::WrapMode::None);
        let editor_scroll = gtk::ScrolledWindow::builder()
            .child(&editor)
            .hexpand(true)
            .vexpand(true)
            .build();
        let editor_panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let fold_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let fold_notice = gtk::Label::new(None);
        fold_notice.set_xalign(0.0);
        fold_notice.set_hexpand(true);
        fold_bar.append(&fold_notice);
        for (label, action) in [
            ("Previous chunk", "win.chunk-previous"),
            ("Next chunk", "win.chunk-next"),
        ] {
            let button = gtk::Button::with_label(label);
            button.set_action_name(Some(action));
            fold_bar.append(&button);
        }
        fold_bar.set_visible(false);
        editor_panel.append(&fold_bar);
        editor_panel.append(&editor_scroll);
        let selection = gtk::SingleSelection::new(None::<gio::ListModel>);
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        let factory = gtk::SignalListItemFactory::new();
        let click_selection = selection.clone();
        factory.connect_setup(move |_, object| {
            let item = object
                .downcast_ref::<gtk::ListItem>()
                .expect("GTK list item");
            let expander = gtk::TreeExpander::new();
            let label = gtk::Label::new(None);
            label.add_css_class("tree-node-label");
            label.set_xalign(0.0);
            label.set_hexpand(true);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            expander.set_child(Some(&label));
            let gesture = gtk::GestureClick::new();
            gesture.set_button(1);
            let weak_expander = expander.downgrade();
            let selection = click_selection.clone();
            gesture.connect_pressed(move |gesture, _, _, _| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                if let Some(expander) = weak_expander.upgrade() {
                    if let Some(row) = expander.list_row() {
                        selection.set_selected(row.position());
                        if row.is_expandable() {
                            row.set_expanded(!row.is_expanded());
                        }
                    }
                }
            });
            label.add_controller(gesture);
            item.set_child(Some(&expander));
        });
        factory.connect_unbind(|_, object| {
            let item = object
                .downcast_ref::<gtk::ListItem>()
                .expect("GTK list item");
            if let Some(expander) = item.child().and_downcast::<gtk::TreeExpander>() {
                if let Some(label) = expander.child().and_downcast::<gtk::Label>() {
                    label.set_attributes(None);
                    label.set_text("");
                }
                expander.set_list_row(None::<&gtk::TreeListRow>);
            }
        });
        let list = gtk::ListView::new(Some(selection.clone()), Some(factory.clone()));
        list.add_css_class("document-tree");
        list.set_single_click_activate(true);
        let tree_scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .width_request(220)
            .vexpand(true)
            .build();
        let page = gtk::Paned::new(gtk::Orientation::Horizontal);
        page.set_start_child(Some(&tree_scroll));
        page.set_end_child(Some(&editor_panel));
        page.set_position(280);
        page.set_shrink_start_child(false);
        let settings = sourceview::SearchSettings::new();
        settings.set_case_sensitive(true);
        settings.set_wrap_around(true);
        let search = sourceview::SearchContext::new(&buffer, Some(&settings));
        search.set_highlight(true);
        let colors = [
            gtk::TextTag::builder()
                .name("json-key")
                .foreground("#9CDCFE")
                .build(),
            gtk::TextTag::builder()
                .name("json-string")
                .foreground("#CE9178")
                .build(),
            gtk::TextTag::builder()
                .name("json-number")
                .foreground("#B5CEA8")
                .build(),
            gtk::TextTag::builder()
                .name("json-literal")
                .foreground("#569CD6")
                .build(),
        ];
        for color in &colors {
            buffer.tag_table().add(color);
        }
        let hidden = gtk::TextTag::builder()
            .name("long-line-hidden")
            .invisible(true)
            .build();
        buffer.tag_table().add(&hidden);
        let title = gtk::Label::new(Some("Untitled"));
        let tab = Rc::new(Tab {
            id,
            page,
            title,
            buffer,
            editor,
            selection,
            list,
            model: RefCell::new(None),
            document: RefCell::new(None),
            document_revision: Cell::new(u64::MAX),
            revision: Arc::new(AtomicU64::new(0)),
            debounce: RefCell::new(None),
            kind: Cell::new(None),
            path: RefCell::new(None),
            dirty: Cell::new(false),
            suppress: Cell::new(false),
            saving: Cell::new(false),
            close_after_save: Cell::new(false),
            status: RefCell::new("Ready".into()),
            search,
            bound_rows: Cell::new(0),
            parse_count: Cell::new(0),
            colors,
            color_job: RefCell::new(None),
            color_revision: Cell::new(u64::MAX),
            syntax: RefCell::new(None),
            color_coverage: Cell::new(None),
            hidden,
            fold_focus: Cell::new(None),
            fold_notice,
        });
        let weak_tab = Rc::downgrade(&tab);
        factory.connect_bind(move |_, object| {
            let Some(tab) = weak_tab.upgrade() else {
                return;
            };
            let item = object
                .downcast_ref::<gtk::ListItem>()
                .expect("GTK list item");
            let Some(row) = item.item().and_downcast::<gtk::TreeListRow>() else {
                return;
            };
            let Some(data) = row.item().and_downcast::<glib::BoxedAnyObject>() else {
                return;
            };
            let node = data.borrow::<tree::NodeRef>();
            let expander = item
                .child()
                .and_downcast::<gtk::TreeExpander>()
                .expect("tree expander");
            let label = expander
                .child()
                .and_downcast::<gtk::Label>()
                .expect("node label");
            label.set_text(&node.label);
            let style = if node.group.is_some() {
                None
            } else {
                Some(&node.document.nodes[node.id].label_style)
            };
            label.set_attributes(Some(&tree_style::attributes(&node.label, style)));
            expander.set_list_row(Some(&row));
            tab.bound_rows.set(tab.bound_rows.get() + 1);
        });
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        header.append(&tab.title);
        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("flat");
        header.append(&close);
        self.tabs.borrow_mut().push(tab.clone());
        let index = self.notebook.append_page(&tab.page, Some(&header));
        self.notebook.set_current_page(Some(index));
        let weak = Rc::downgrade(self);
        close.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.close_tab(id);
            }
        });
        let weak = Rc::downgrade(self);
        let weak_tab = Rc::downgrade(&tab);
        tab.buffer.connect_changed(move |_| {
            let (Some(app), Some(tab)) = (weak.upgrade(), weak_tab.upgrade()) else {
                return;
            };
            if tab.suppress.get() {
                return;
            }
            tab.revision.fetch_add(1, Ordering::Relaxed);
            tab.dirty.set(true);
            tab.update_title();
            tab.list.set_sensitive(false);
            if let Some(timer) = tab.debounce.borrow_mut().take() {
                timer.remove();
            }
            app.message(&tab, "Changes pending analysis…");
            let weak = Rc::downgrade(&app);
            let weak_tab = Rc::downgrade(&tab);
            let timer = glib::timeout_add_local_once(Duration::from_millis(350), move || {
                if let (Some(app), Some(tab)) = (weak.upgrade(), weak_tab.upgrade()) {
                    tab.debounce.borrow_mut().take();
                    app.parse(&tab, None);
                }
            });
            tab.debounce.replace(Some(timer));
        });
        let weak = Rc::downgrade(self);
        let weak_tab = Rc::downgrade(&tab);
        tab.buffer.connect_mark_set(move |_, _, mark| {
            if let (Some(app), Some(tab)) = (weak.upgrade(), weak_tab.upgrade()) {
                if !tab.suppress.get() && mark.name().as_deref() == Some("insert") {
                    app.fold_at_cursor(&tab);
                }
                if app.active().is_some_and(|active| active.id == tab.id) {
                    app.update_position(&tab);
                }
            }
        });
        let weak = Rc::downgrade(self);
        let weak_tab = Rc::downgrade(&tab);
        tab.selection.connect_selected_notify(move |selection| {
            let (Some(app), Some(tab)) = (weak.upgrade(), weak_tab.upgrade()) else {
                return;
            };
            if tab.document_revision.get() != tab.revision() {
                return;
            }
            let Some(model) = tab.model.borrow().clone() else {
                return;
            };
            if let Some(node) = tree::node_at(&model, selection.selected()) {
                if node.group.is_some() {
                    return;
                }
                let range = &node.document.nodes[node.id].chars;
                tab.select_range(range.start, range.end);
                app.message(
                    &tab,
                    format!("Selected: {}", node.document.nodes[node.id].label),
                );
            }
        });
        let weak = Rc::downgrade(self);
        let weak_tab = Rc::downgrade(&tab);
        tab.list.connect_activate(move |_, position| {
            let (Some(app), Some(tab)) = (weak.upgrade(), weak_tab.upgrade()) else {
                return;
            };
            if tab.document_revision.get() != tab.revision() {
                return;
            }
            if let Some(model) = tab.model.borrow().as_ref() {
                if let Some(row) = model.row(position) {
                    if row.is_expandable() {
                        row.set_expanded(!row.is_expanded());
                    }
                }
            }
            app.refresh();
        });
        let weak = Rc::downgrade(self);
        for adjustment in [editor_scroll.hadjustment(), editor_scroll.vadjustment()] {
            let weak = Rc::downgrade(self);
            let weak_tab = Rc::downgrade(&tab);
            adjustment.connect_value_changed(move |_| {
                if let (Some(app), Some(tab)) = (weak.upgrade(), weak_tab.upgrade()) {
                    app.paint_colors(&tab);
                }
            });
        }
        tab.search
            .connect_notify_local(Some("occurrences-count"), move |_, _| {
                if let Some(app) = weak.upgrade() {
                    app.refresh_search();
                }
            });
        tab.editor.grab_focus();
        tab
    }

    fn submit(&self, tab: &Tab, task: Task) {
        let job = Job {
            tab: tab.id,
            revision: tab.revision(),
            current: tab.revision.clone(),
            task,
        };
        if self.jobs.send(job).is_err() {
            self.message(tab, "Error: parsing worker unavailable");
        }
    }

    fn parse(&self, tab: &Tab, explicit: Option<Kind>) {
        if let Some(timer) = tab.debounce.borrow_mut().take() {
            timer.remove();
        }
        let text: Arc<str> = tab.text().into();
        let kind = explicit
            .or(tab.kind.get())
            .unwrap_or_else(|| Kind::detect(&text));
        if explicit.is_some() {
            tab.revision.fetch_add(1, Ordering::Relaxed);
            tab.list.set_sensitive(false);
        }
        tab.set_kind(kind);
        self.message(tab, "Parsing…");
        self.submit(tab, Task::Parse(text, kind));
    }

    fn open(self: &Rc<Self>, path: PathBuf) {
        let tab = match self.active() {
            Some(tab)
                if !tab.dirty.get()
                    && tab.path.borrow().is_none()
                    && tab.buffer.char_count() == 0
                    && tab.editor.is_editable() =>
            {
                tab
            }
            _ => self.new_tab(),
        };
        tab.revision.fetch_add(1, Ordering::Relaxed);
        tab.editor.set_editable(false);
        self.message(&tab, format!("Opening {}…", path.display()));
        self.submit(&tab, Task::Open(path));
    }

    fn install_document(&self, tab: &Tab, revision: u64, parsed: Result<Arc<Document>, String>) {
        tab.parse_count.set(tab.parse_count.get() + 1);
        match parsed {
            Ok(document) => {
                let model = tree::model(document.clone());
                // Expand only the first root; deeper branches stay lazy.
                if let Some(row) = model.row(0) {
                    row.set_expanded(true);
                }
                tab.model.replace(Some(model.clone()));
                tab.document.replace(Some(document.clone()));
                tab.document_revision.set(revision);
                tab.selection.set_model(Some(&model));
                tab.list.set_sensitive(true);
                self.message(
                    tab,
                    format!("{} nodes · analysis up to date", document.nodes.len()),
                );
            }
            Err(error) => {
                tab.list.set_sensitive(false);
                self.message(tab, error);
            }
        }
    }

    fn accept(self: &Rc<Self>, reply: Reply) {
        let Some(tab) = self
            .tabs
            .borrow()
            .iter()
            .find(|tab| tab.id == reply.tab)
            .cloned()
        else {
            return;
        };
        if reply.revision != tab.revision() && !matches!(reply.output, Output::Saved(..)) {
            return;
        }
        match reply.output {
            Output::Parsed(parsed, colors) => {
                self.install_document(&tab, reply.revision, parsed);
                self.install_colors(&tab, reply.revision, colors);
            }
            Output::Opened(path, kind, parsed, content, colors, adapted) => {
                tab.editor.set_editable(true);
                if let Some(content) = content {
                    tab.suppress.set(true);
                    tab.set_kind(kind);
                    tab.buffer.begin_irreversible_action();
                    tab.buffer.set_text(&content);
                    tab.buffer.end_irreversible_action();
                    tab.suppress.set(false);
                    tab.path.replace(Some(path));
                    tab.dirty.set(adapted);
                    tab.update_title();
                    self.install_document(&tab, reply.revision, parsed);
                    self.install_colors(&tab, reply.revision, colors);
                    if adapted {
                        self.message(
                            &tab,
                            "JSON auto-formatted due to an extreme line (>16 KB) · unsaved changes",
                        );
                    }
                } else if let Err(error) = parsed {
                    self.message(&tab, error);
                }
            }
            Output::Formatted(result) => match result {
                Ok(text) => {
                    self.replace(&tab, 0, tab.buffer.char_count(), &text);
                    self.message(&tab, "Document formatted; mixed XML content is preserved without added whitespace");
                }
                Err(error) => self.message(&tab, format!("Formatting error: {error}")),
            },
            Output::Decoded(result, start, end) => match result {
                Ok(Some(text)) => {
                    self.replace(&tab, start, end, &text);
                    self.message(&tab, "Selection decoded");
                }
                Ok(None) => self.message(&tab, "No escaped content to decode"),
                Err(error) => self.message(&tab, format!("Decode error: {error}")),
            },
            Output::Saved(path, result) => {
                tab.saving.set(false);
                match result {
                    Ok(()) => {
                        tab.path.replace(Some(path));
                        let unchanged = reply.revision == tab.revision();
                        tab.dirty.set(!unchanged);
                        tab.update_title();
                        self.message(
                            &tab,
                            if unchanged {
                                "Saved"
                            } else {
                                "Saved; newer edits are still pending"
                            },
                        );
                        if tab.close_after_save.replace(false) && unchanged {
                            self.remove_tab(&tab);
                        }
                    }
                    Err(error) => {
                        tab.close_after_save.set(false);
                        self.message(&tab, error);
                    }
                }
            }
        }
        self.refresh();
    }

    fn replace(&self, tab: &Tab, start: i32, end: i32, text: &str) {
        tab.buffer.begin_user_action();
        let mut first = tab.buffer.iter_at_offset(start);
        let mut last = tab.buffer.iter_at_offset(end);
        tab.buffer.delete(&mut first, &mut last);
        tab.buffer.insert(&mut first, text);
        tab.buffer.end_user_action();
    }

    fn install_colors(&self, tab: &Rc<Tab>, revision: u64, colors: Option<Highlight>) {
        tab.color_coverage.set(None);
        tab.syntax.replace(colors.map(|colors| (revision, colors)));
        tab.fold_focus.set(None);
        self.fold_at_cursor(tab);
        if tab.syntax.borrow().is_none() {
            tab.color_revision.set(revision);
        } else {
            self.paint_colors(tab);
        }
    }

    fn paint_colors(&self, tab: &Rc<Tab>) {
        if tab.color_job.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(tab);
        let job = glib::timeout_add_local_once(Duration::from_millis(16), move || {
            let Some(tab) = weak.upgrade() else {
                return;
            };
            tab.color_job.borrow_mut().take();
            let data = tab.syntax.borrow();
            let Some((revision, highlight)) = data.as_ref() else {
                return;
            };
            if *revision != tab.revision() || tab.kind.get() != Some(Kind::Json) {
                return;
            }
            let rect = tab.editor.visible_rect();
            if rect.width() <= 0 || rect.height() <= 0 {
                return;
            }
            let (first_line, first_y) = tab.editor.line_at_y(rect.y());
            let (mut last_line, last_y) = tab.editor.line_at_y(rect.y() + rect.height());
            let first = tab
                .editor
                .iter_at_location(rect.x(), first_y + 1)
                .unwrap_or(first_line);
            last_line.forward_to_line_end();
            let last = tab
                .editor
                .iter_at_location(rect.x() + rect.width(), last_y + 1)
                .unwrap_or(last_line);
            let (start, end) = (
                first.offset().saturating_sub(64),
                (last.offset() + 64).min(tab.buffer.char_count()),
            );
            if tab.color_coverage.get().is_some_and(|(r, a, b)| {
                r == *revision && a <= first.offset() && b >= last.offset()
            }) {
                return;
            }
            for tag in &tab.colors {
                tab.buffer
                    .remove_tag(tag, &tab.buffer.start_iter(), &tab.buffer.end_iter());
            }
            // Only visible spans are materialized. Thousands of GTK tags on one
            // huge line are costly even when tokenization is linear.
            let first_span = highlight
                .spans
                .partition_point(|span| span.range.end <= start);
            for span in highlight.spans[first_span..]
                .iter()
                .take_while(|span| span.range.start < end)
            {
                let tag = &tab.colors[match span.style {
                    Style::Key => 0,
                    Style::String => 1,
                    Style::Number => 2,
                    Style::Literal => 3,
                }];
                tab.buffer.apply_tag(
                    tag,
                    &tab.buffer.iter_at_offset(span.range.start.max(start)),
                    &tab.buffer.iter_at_offset(span.range.end.min(end)),
                );
            }
            tab.color_coverage.set(Some((*revision, start, end)));
            tab.color_revision.set(*revision);
        });
        tab.color_job.replace(Some(job));
    }

    fn fold_at_cursor(&self, tab: &Rc<Tab>) {
        let syntax = tab.syntax.borrow();
        let Some((revision, highlight)) = syntax.as_ref() else {
            return;
        };
        if *revision != tab.revision() {
            return;
        }
        let cursor = tab.buffer.cursor_position();
        let inside = highlight
            .long_lines
            .iter()
            .find(|range| range.contains(&cursor));
        let focus = inside.map(|range| (cursor - range.start) / 512 * 512 + range.start);
        if tab.fold_focus.get() == focus
            && tab.fold_notice.parent().is_some_and(|p| p.is_visible())
                == !highlight.long_lines.is_empty()
        {
            return;
        }
        tab.fold_focus.set(focus);
        tab.buffer.remove_tag(
            &tab.hidden,
            &tab.buffer.start_iter(),
            &tab.buffer.end_iter(),
        );
        for range in &highlight.long_lines {
            let start = if range.contains(&cursor) {
                focus.unwrap_or(range.start)
            } else {
                range.start
            };
            let end = (start + 512).min(range.end);
            if start > range.start {
                tab.buffer.apply_tag(
                    &tab.hidden,
                    &tab.buffer.iter_at_offset(range.start),
                    &tab.buffer.iter_at_offset(start),
                );
            }
            if end < range.end {
                tab.buffer.apply_tag(
                    &tab.hidden,
                    &tab.buffer.iter_at_offset(end),
                    &tab.buffer.iter_at_offset(range.end),
                );
            }
        }
        if let Some(parent) = tab.fold_notice.parent() {
            parent.set_visible(!highlight.long_lines.is_empty());
        }
        tab.fold_notice
            .set_visible(!highlight.long_lines.is_empty());
        tab.fold_notice.set_text(&if let Some(range) = inside {
            format!(
                "Chunked view · {}–{} of {} characters · hidden text is preserved",
                focus.unwrap() - range.start + 1,
                (focus.unwrap() + 512).min(range.end) - range.start,
                range.end - range.start
            )
        } else {
            format!(
                "{} long lines in 512-character chunks · hidden text is preserved",
                highlight.long_lines.len()
            )
        });
        tab.color_coverage.set(None);
        drop(syntax);
        self.paint_colors(tab);
    }

    fn next_chunk(&self, backwards: bool) {
        let Some(tab) = self.active() else {
            return;
        };
        let syntax = tab.syntax.borrow();
        let Some((revision, highlight)) = syntax.as_ref() else {
            return;
        };
        if *revision != tab.revision() {
            return;
        }

        let cursor = tab.buffer.cursor_position();
        let Some(range) = highlight
            .long_lines
            .iter()
            .find(|range| range.contains(&cursor))
            .or(highlight.long_lines.first())
        else {
            return;
        };
        let current = tab.fold_focus.get().unwrap_or(range.start);
        let offset = if backwards {
            (current - 512).max(range.start)
        } else {
            (current + 512).min(range.end - 1)
        };
        drop(syntax);
        tab.buffer.place_cursor(&tab.buffer.iter_at_offset(offset));
        self.fold_at_cursor(&tab);
        let mut cursor = tab.buffer.iter_at_offset(offset);
        tab.editor.scroll_to_iter(&mut cursor, 0.1, false, 0.0, 0.0);
    }

    fn editing_search(&self) -> bool {
        gtk::prelude::GtkWindowExt::focus(&self.window).is_some_and(|focus| {
            focus == self.search_input || focus.is_ancestor(&self.search_input)
        })
    }

    fn copy(&self, cut: bool) {
        if self.editing_search() {
            if let Some((start, end)) = self.search_input.selection_bounds() {
                let text: String = self
                    .search_input
                    .text()
                    .chars()
                    .skip(start as usize)
                    .take((end - start) as usize)
                    .collect();
                self.window.clipboard().set_text(&text);
                if cut {
                    self.search_input.delete_text(start, end);
                }
            }
            return;
        }
        if let Some(tab) = self.active() {
            if let Some((start, end)) = tab.buffer.selection_bounds() {
                self.window
                    .clipboard()
                    .set_text(&tab.buffer.text(&start, &end, true));
                if cut {
                    self.replace(&tab, start.offset(), end.offset(), "");
                }
            } else {
                self.message(&tab, "Select text to copy");
            }
        }
    }

    fn refresh(&self) {
        if let Some(tab) = self.active() {
            self.window
                .set_title(Some(&format!("XML/JSON Viewer Rust — {}", tab.title())));
            self.status.set_text(&tab.status.borrow());
            self.update_position(&tab);
            let query = self.search_input.text();
            tab.search.settings().set_search_text(if query.is_empty() {
                None
            } else {
                Some(query.as_str())
            });
            self.refresh_search();
            self.paint_colors(&tab);
        }
    }

    fn update_position(&self, tab: &Tab) {
        let iter = tab.buffer.iter_at_offset(tab.buffer.cursor_position());
        self.position.set_text(&format!(
            "Ln {}, Col {}",
            iter.line() + 1,
            iter.line_offset() + 1
        ));
    }

    fn refresh_search(&self) {
        if let Some(tab) = self.active() {
            let count = tab.search.occurrences_count();
            self.match_label
                .set_text(&if self.search_input.text().is_empty() {
                    String::new()
                } else if count < 0 {
                    "Searching…".into()
                } else {
                    format!("{count} matches")
                });
        }
    }

    fn hide_search(&self) {
        self.search_bar.set_visible(false);
        if let Some(tab) = self.active() {
            tab.editor.grab_focus();
        }
    }

    fn find(&self, backwards: bool) {
        let Some(tab) = self.active() else {
            return;
        };
        let (start, end) = tab.buffer.selection_bounds().unwrap_or_else(|| {
            let i = tab.buffer.iter_at_offset(tab.buffer.cursor_position());
            (i, i)
        });
        let found = if backwards {
            tab.search.backward(&start)
        } else {
            tab.search.forward(&end)
        };
        if let Some((first, last, _)) = found {
            tab.select_range(first.offset(), last.offset());
        } else {
            self.message(&tab, "No matches found");
        }
    }

    fn select(&self) {
        self.select_node(false);
    }

    fn select_node(&self, expand: bool) {
        let Some(tab) = self.active() else {
            return;
        };
        if tab.document_revision.get() != tab.revision() {
            self.message(
                &tab,
                "Wait for parsing to finish before synchronizing the selection",
            );
            return;
        }
        let Some(document) = tab.document.borrow().clone() else {
            return;
        };
        let Some(id) = document.at_cursor(tab.buffer.cursor_position()) else {
            self.message(&tab, "No element at this position");
            return;
        };
        if let Some(model) = tab.model.borrow().as_ref() {
            if let Some(position) = tree::reveal(model, &document, id) {
                if expand {
                    if let Some(row) = model.row(position) {
                        row.set_expanded(true);
                    }
                }
                tab.selection.set_selected(position);
                tab.list
                    .scroll_to(position, gtk::ListScrollFlags::FOCUS, None);
            }
        }
        let range = &document.nodes[id].chars;
        tab.select_range(range.start, range.end);
        self.message(&tab, format!("Selected: {}", document.nodes[id].label));
    }

    fn chooser(self: &Rc<Self>, save: bool, tab: Option<Rc<Tab>>) {
        let dialog = gtk::FileChooserNative::new(
            Some(if save { "Save as" } else { "Open XML/JSON" }),
            Some(&self.window),
            if save {
                gtk::FileChooserAction::Save
            } else {
                gtk::FileChooserAction::Open
            },
            Some(if save { "Save" } else { "Open" }),
            Some("Cancel"),
        );
        for (name, patterns) in [
            ("XML and JSON", vec!["*.xml", "*.json"]),
            ("All files", vec!["*"]),
        ] {
            let filter = gtk::FileFilter::new();
            filter.set_name(Some(name));
            for pattern in patterns {
                filter.add_pattern(pattern);
            }
            dialog.add_filter(&filter);
        }
        dialog.set_select_multiple(!save);
        if save {
            if let Some(tab) = &tab {
                dialog.set_current_name(&tab.title());
            }
        }
        let weak = Rc::downgrade(self);
        dialog.connect_response(move |dialog, response| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            if response == gtk::ResponseType::Accept {
                if save {
                    if let (Some(tab), Some(path)) = (&tab, dialog.file().and_then(|f| f.path())) {
                        app.save_to(tab, path);
                    } else {
                        app.status.set_text("No local file selected");
                    }
                } else {
                    let files = dialog.files();
                    for index in 0..files.n_items() {
                        if let Some(path) = files
                            .item(index)
                            .and_downcast::<gio::File>()
                            .and_then(|f| f.path())
                        {
                            app.open(path);
                        } else {
                            app.status.set_text("Only local files are supported");
                        }
                    }
                }
            } else if let Some(tab) = &tab {
                tab.close_after_save.set(false);
            }
            dialog.destroy();
        });
        dialog.show();
    }

    fn save(self: &Rc<Self>, save_as: bool) {
        let Some(tab) = self.active() else {
            return;
        };
        if !tab.editor.is_editable() {
            self.message(&tab, "Wait for the file to finish opening");
            return;
        }
        let path = tab.path.borrow().clone();
        if !save_as {
            if let Some(path) = path {
                self.save_to(&tab, path);
                return;
            }
        }
        self.chooser(true, Some(tab));
    }

    fn save_to(&self, tab: &Tab, path: PathBuf) {
        if tab.saving.replace(true) {
            self.message(tab, "A save is already in progress");
            return;
        }
        self.message(tab, "Saving…");
        let job = Job {
            tab: tab.id,
            revision: tab.revision(),
            current: tab.revision.clone(),
            task: Task::Save(path, tab.text()),
        };
        if self.jobs.send(job).is_err() {
            tab.saving.set(false);
            tab.close_after_save.set(false);
            self.message(tab, "Error: saving worker unavailable");
        }
    }

    fn close_tab(self: &Rc<Self>, id: u64) {
        let Some(tab) = self.tabs.borrow().iter().find(|t| t.id == id).cloned() else {
            return;
        };
        if tab.saving.get() {
            self.message(&tab, "Wait for the save to finish");
            return;
        }
        if !tab.dirty.get() {
            self.remove_tab(&tab);
            return;
        }
        let dialog = gtk::MessageDialog::builder()
            .transient_for(&self.window)
            .modal(true)
            .text(format!("Save changes to {}?", tab.title()))
            .build();
        dialog.add_button("Cancel", gtk::ResponseType::Cancel);
        dialog.add_button("Discard", gtk::ResponseType::Reject);
        dialog.add_button("Save", gtk::ResponseType::Accept);
        dialog.set_default_response(gtk::ResponseType::Cancel);
        let weak = Rc::downgrade(self);
        dialog.connect_response(move |dialog, response| {
            dialog.close();
            let Some(app) = weak.upgrade() else {
                return;
            };
            if response == gtk::ResponseType::Reject {
                app.remove_tab(&tab);
            } else if response == gtk::ResponseType::Accept {
                tab.close_after_save.set(true);
                let path = tab.path.borrow().clone();
                if let Some(path) = path {
                    app.save_to(&tab, path);
                } else {
                    app.chooser(true, Some(tab.clone()));
                }
            }
        });
        dialog.present();
    }

    fn remove_tab(self: &Rc<Self>, tab: &Tab) {
        tab.revision.fetch_add(1, Ordering::Relaxed);
        if let Some(timer) = tab.debounce.borrow_mut().take() {
            timer.remove();
        }
        if let Some(timer) = tab.color_job.borrow_mut().take() {
            timer.remove();
        }
        if let Some(index) = self.notebook.page_num(&tab.page) {
            self.notebook.remove_page(Some(index));
        }
        self.tabs.borrow_mut().retain(|t| t.id != tab.id);
        if self.tabs.borrow().is_empty() {
            self.new_tab();
        }
        self.refresh();
    }

    fn actions(self: &Rc<Self>) {
        for (name, keys) in [
            ("new", vec!["<Control>t"]),
            ("open", vec!["<Control>o"]),
            ("save", vec!["<Control>s"]),
            ("save-as", vec!["<Control><Shift>s"]),
            ("close-tab", vec!["<Control>w"]),
            ("quit", vec!["<Control>q"]),
            ("select", vec!["<Control>e"]),
            ("reveal", vec!["<Control>l"]),
            ("chunk-previous", vec![]),
            ("chunk-next", vec![]),
            ("copy", vec!["<Control>c"]),
            ("cut", vec!["<Control>x"]),
            ("select-all", vec!["<Control>a"]),
            ("format", vec!["<Control>i"]),
            ("search", vec!["<Control>f"]),
            ("next", vec!["F3"]),
            ("previous", vec!["<Shift>F3"]),
            ("hide-search", vec!["Escape"]),
            ("undo", vec!["<Control>z"]),
            ("redo", vec!["<Control><Shift>z"]),
            ("parse-xml", vec![]),
            ("parse-json", vec![]),
            ("decode", vec![]),
        ] {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                let Some(app) = weak.upgrade() else {
                    return;
                };
                match name {
                    "new" => {
                        app.new_tab();
                    }
                    "open" => app.chooser(false, None),
                    "save" => app.save(false),
                    "save-as" => app.save(true),
                    "close-tab" => {
                        if let Some(tab) = app.active() {
                            app.close_tab(tab.id);
                        }
                    }
                    "quit" => app.window.close(),
                    "search" => {
                        if app.search_bar.is_visible() {
                            app.hide_search();
                        } else {
                            app.search_bar.set_visible(true);
                            app.search_input.grab_focus();
                        }
                    }
                    "hide-search" => app.hide_search(),
                    "next" => app.find(false),
                    "previous" => app.find(true),
                    "select" => app.select(),
                    "reveal" => app.select_node(true),
                    "chunk-previous" => app.next_chunk(true),
                    "chunk-next" => app.next_chunk(false),
                    "copy" => app.copy(false),
                    "cut" => app.copy(true),
                    "select-all" => {
                        if app.editing_search() {
                            app.search_input.select_region(0, -1);
                        } else if let Some(tab) = app.active() {
                            tab.buffer
                                .select_range(&tab.buffer.start_iter(), &tab.buffer.end_iter());
                        }
                    }
                    "parse-xml" | "parse-json" => {
                        if let Some(tab) = app.active() {
                            app.parse(
                                &tab,
                                Some(if name == "parse-xml" {
                                    Kind::Xml
                                } else {
                                    Kind::Json
                                }),
                            );
                        }
                    }
                    "format" => {
                        if let Some(tab) = app.active() {
                            let text: Arc<str> = tab.text().into();
                            let kind = tab.kind.get().unwrap_or_else(|| Kind::detect(&text));
                            tab.set_kind(kind);
                            app.message(&tab, "Formatting in the background…");
                            app.submit(&tab, Task::Format(text, kind));
                        }
                    }
                    "decode" => {
                        if let Some(tab) = app.active() {
                            if let Some((start, end)) = tab.buffer.selection_bounds() {
                                let text = tab.buffer.text(&start, &end, true).to_string();
                                app.submit(&tab, Task::Decode(text, start.offset(), end.offset()));
                            } else {
                                app.message(&tab, "Select text first (Ctrl+E)");
                            }
                        }
                    }
                    "undo" => {
                        if let Some(tab) = app.active() {
                            if tab.buffer.can_undo() {
                                tab.buffer.undo();
                            }
                        }
                    }
                    "redo" => {
                        if let Some(tab) = app.active() {
                            if tab.buffer.can_redo() {
                                tab.buffer.redo();
                            }
                        }
                    }
                    _ => unreachable!(),
                }
            });
            self.window.add_action(&action);
            self.application
                .set_accels_for_action(&format!("win.{name}"), &keys);
        }
    }
}

fn ensure(application: &gtk::Application, holder: &RefCell<Option<Rc<App>>>) -> Rc<App> {
    if let Some(app) = holder.borrow().clone() {
        return app;
    }
    let app = App::new(application);
    holder.replace(Some(app.clone()));
    app
}

pub fn run() {
    gio::resources_register_include!("viewer.gresource")
        .expect("Embedded viewer icon must be valid");
    let application = gtk::Application::builder()
        .application_id("io.github.xmljsonviewer.Viewer")
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    let holder = Rc::new(RefCell::new(None));
    let copy = holder.clone();
    application.connect_activate(move |application| {
        let app = ensure(application, &copy);
        app.window.present();
    });
    let copy = holder.clone();
    application.connect_open(move |application, files, _| {
        let app = ensure(application, &copy);
        for file in files {
            if let Some(path) = file.path() {
                app.open(path);
            } else {
                app.status.set_text("Only local files are supported");
            }
        }
        app.window.present();
    });
    let args: Vec<String> = std::env::args().collect();
    if args
        .get(1)
        .is_some_and(|arg| arg == "--gui-benchmark" || arg == "--ui-smoke")
    {
        let smoke = args[1] == "--ui-smoke";
        let path = args.get(2).map(PathBuf::from);
        let copy = holder.clone();
        application.connect_activate(move |application| {
            let app = ensure(application, &copy);
            if smoke {
                smoke_test(&app);
            } else if let Some(path) = &path {
                gui_benchmark(&app, path.clone());
            } else {
                eprintln!("Usage: --gui-benchmark FILE");
                std::process::exit(1);
            }
        });
        application.run_with_args(&[args[0].as_str()]);
    } else {
        application.run();
    }
}

fn gui_benchmark(app: &Rc<App>, path: PathBuf) {
    let start = Instant::now();
    let ready = Cell::new(None::<Instant>);
    let last = Rc::new(Cell::new(Instant::now()));
    let max_gap = Rc::new(Cell::new(Duration::ZERO));
    let weak = Rc::downgrade(app);
    glib::timeout_add_local(Duration::from_millis(10), move || {
        let now = Instant::now();
        max_gap.set(max_gap.get().max(now.duration_since(last.replace(now))));
        let Some(app) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let Some(tab) = app.active() else {
            return glib::ControlFlow::Continue;
        };
        if tab.document_revision.get() == tab.revision()
            && tab.color_revision.get() == tab.revision()
            && tab.bound_rows.get() > 0
        {
            let since = ready.get().unwrap_or_else(|| {
                let now = Instant::now();
                ready.set(Some(now));
                now
            });
            if since.elapsed() < Duration::from_millis(500) {
                return glib::ControlFlow::Continue;
            }
            let begin = Instant::now();
            tab.buffer
                .ensure_highlight(&tab.buffer.start_iter(), &tab.buffer.end_iter());
            let mut colored = 0;
            let mut iter = tab.buffer.start_iter();
            for _ in 0..1000 {
                if iter
                    .tags()
                    .iter()
                    .any(|tag| tag.foreground_rgba().is_some())
                {
                    colored += 1;
                }
                if !iter.forward_char() {
                    break;
                }
            }
            eprintln!(
                "language={} style={} colored_first_1000={} ensure_highlight_ms={:.3}",
                tab.buffer
                    .language()
                    .map(|l| l.id().to_string())
                    .unwrap_or_default(),
                tab.buffer
                    .style_scheme()
                    .map(|s| s.id().to_string())
                    .unwrap_or_default(),
                colored,
                begin.elapsed().as_secs_f64() * 1000.0
            );
            assert!(colored > 0, "The visible JSON must retain syntax colors");
            if tab.kind.get() == Some(Kind::Json) {
                let document = tab.document.borrow().clone().unwrap();
                let mut containers = std::collections::HashMap::new();
                for (id, node) in document.nodes.iter().enumerate() {
                    if !node.children.is_empty() && node.parent.is_some() {
                        containers
                            .entry(node.label.split(": ").next().unwrap_or(""))
                            .or_insert_with(Vec::new)
                            .push(id);
                    }
                }
                let ids = containers
                    .values()
                    .find(|ids| ids.len() > 1)
                    .cloned()
                    .unwrap_or_else(|| vec![document.roots[0]]);
                for id in ids.iter().rev().take(2) {
                    tab.buffer.place_cursor(
                        &tab.buffer
                            .iter_at_offset(document.nodes[*id].chars.start + 1),
                    );
                    gtk::prelude::WidgetExt::activate_action(&app.window, "win.reveal", None)
                        .unwrap();
                    let model = tab.model.borrow().clone().unwrap();
                    let position = tab.selection.selected();
                    assert_eq!(
                        tree::node_at(&model, position).unwrap().id,
                        *id,
                        "Ctrl+L must locate exact repeated key"
                    );
                    assert!(model.row(position).unwrap().is_expanded());
                    tab.list.emit_by_name::<()>("activate", &[&position]);
                    assert!(
                        !model.row(position).unwrap().is_expanded(),
                        "Click text must collapse"
                    );
                    tab.list.emit_by_name::<()>("activate", &[&position]);
                    assert!(
                        model.row(position).unwrap().is_expanded(),
                        "Click text must expand"
                    );
                }
                eprintln!(
                    "REAL_TREE_OK repeated_containers={} ctrl_l_exact_node text_click_toggle",
                    ids.len()
                );
                if let Some((_, highlight)) = tab.syntax.borrow().as_ref() {
                    if let Some(range) = highlight.long_lines.first() {
                        let offset = range.start + 512;
                        let text = tab.text();
                        tab.buffer.place_cursor(&tab.buffer.iter_at_offset(offset));
                        assert!(!tab.buffer.iter_at_offset(offset).has_tag(&tab.hidden));
                        app.next_chunk(false);
                        assert!(tab.buffer.cursor_position() >= offset + 512);
                        assert!(
                            !tab.buffer
                                .iter_at_offset(tab.buffer.cursor_position())
                                .has_tag(&tab.hidden)
                        );
                        app.next_chunk(true);
                        assert_eq!(tab.buffer.cursor_position(), offset);
                        assert_eq!(tab.text(), text);
                        eprintln!("REAL_CHUNKS_OK cursor_next_previous_source_preserved");
                    }
                }
            }
            let rss = crate::peak_rss_kib().expect("Linux VmHWM measurement");
            println!(
                "gui_ms={:.3} nodes={} bound_rows={} max_tick_gap_ms={:.3} peak_rss_kib={}",
                start.elapsed().as_secs_f64() * 1000.0,
                tab.document.borrow().as_ref().map_or(0, |d| d.nodes.len()),
                tab.bound_rows.get(),
                max_gap.get().as_secs_f64() * 1000.0,
                rss
            );
            app.closing.set(true);
            app.window.close();
            return glib::ControlFlow::Break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            eprintln!("GUI benchmark did not finish: {}", tab.status.borrow());
            std::process::exit(1);
        }
        glib::ControlFlow::Continue
    });
    app.open(path);
}

fn smoke_test(app: &Rc<App>) {
    assert_eq!(app.status.text().as_str(), "Ready");
    assert_eq!(app.active().unwrap().title(), "Untitled");
    assert_eq!(
        app.application.application_id().as_deref(),
        Some("io.github.xmljsonviewer.Viewer")
    );
    let menus = app
        .window
        .child()
        .unwrap()
        .first_child()
        .and_downcast::<gtk::PopoverMenuBar>()
        .unwrap()
        .menu_model()
        .unwrap();
    let file_label = menus
        .item_attribute_value(0, "label", Some(glib::VariantTy::STRING))
        .unwrap();
    let edit_label = menus
        .item_attribute_value(1, "label", Some(glib::VariantTy::STRING))
        .unwrap();
    assert_eq!(file_label.str(), Some("File"));
    assert_eq!(edit_label.str(), Some("Edit"));
    let theme = gtk::IconTheme::for_display(&gdk::Display::default().unwrap());
    assert!(theme.has_icon("xmljson-viewer-rust"));
    assert_eq!(
        app.window.icon_name().as_deref(),
        Some("xmljson-viewer-rust")
    );
    let tab = app.active().expect("initial tab");
    tab.buffer
        .set_text(r#"{"é":"😀","a":[{"x":1},{"x":2}],"auditData":"{\"k\":3}"}"#);
    app.parse(&tab, Some(Kind::Json));
    let weak: Weak<App> = Rc::downgrade(app);
    let start = Instant::now();
    let stage = Cell::new(0);
    let temp = std::env::temp_dir().join(format!("xmljson-rust-smoke-{}.json", std::process::id()));
    let saved = temp.clone();
    glib::timeout_add_local(Duration::from_millis(50), move || {
        let Some(app) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if start.elapsed() > Duration::from_secs(20) {
            eprintln!(
                "UI smoke timeout at stage {}: {}",
                stage.get(),
                tab.status.borrow()
            );
            std::process::exit(1);
        }

        match stage.get() {
            0 if tab.document_revision.get() == tab.revision() => {
                assert!(tab.kind.get() == Some(Kind::Json));
                let doc = tab.document.borrow().clone().unwrap();
                let id = doc.nodes.iter().position(|n| n.label == "x: 2").unwrap();
                let model = tab.model.borrow().clone().unwrap();
                let position = tree::reveal(&model, &doc, id).unwrap();
                tab.selection.set_selected(position);
                let (first, last) = tab.buffer.selection_bounds().unwrap();
                assert_eq!(tab.buffer.text(&first, &last, true), r#""x":2"#);
                app.search_input.set_text("x");
                tab.search.settings().set_search_text(Some("x"));
                app.find(false);
                let second = app.new_tab();
                second.buffer.set_text("<r><a/><a>text</a></r>");
                app.parse(&second, Some(Kind::Xml));
                stage.set(1);
            }
            1 if app
                .active()
                .is_some_and(|t| t.document_revision.get() == t.revision()) =>
            {
                assert_eq!(app.tabs.borrow().len(), 2);
                let second = app.active().unwrap();
                second.buffer.place_cursor(&second.buffer.iter_at_offset(6));
                app.select();
                let (first, last) = second.buffer.selection_bounds().unwrap();
                assert_eq!(second.buffer.text(&first, &last, true), "<a/>");
                let mut large = String::from("<r>");
                for id in 0..1100 {
                    large.push_str(&format!("<e id=\"{id}\"/>"));
                }
                large.push_str("</r>");
                second.buffer.set_text(&large);
                app.parse(&second, Some(Kind::Xml));
                stage.set(5);
            }
            5 if app
                .active()
                .is_some_and(|t| t.document_revision.get() == t.revision()) =>
            {
                let second = app.active().unwrap();
                let document = second.document.borrow().clone().unwrap();
                let model = second.model.borrow().clone().unwrap();
                assert_eq!(model.n_items(), 4, "root plus three lazy groups");
                let id = document.nodes.len() - 1;
                let position = tree::reveal(&model, &document, id).unwrap();
                second.selection.set_selected(position);
                let (first, last) = second.buffer.selection_bounds().unwrap();
                assert_eq!(second.buffer.text(&first, &last, true), "<e id=\"1099\"/>");
                assert!(model.n_items() < 100, "only target group is materialized");
                app.notebook.set_current_page(Some(0));
                let before = tab.parse_count.get();
                for _ in 0..100 {
                    let mut end = tab.buffer.end_iter();
                    tab.buffer.insert(&mut end, " ");
                }
                assert_eq!(
                    tab.parse_count.get(),
                    before,
                    "no parsing during keystrokes"
                );
                assert!(!tab.list.is_sensitive());
                stage.set(2);
            }
            2 if tab.document_revision.get() == tab.revision() => {
                assert_eq!(tab.parse_count.get(), 2, "100 edits coalesced to one parse");
                let doc = tab.document.borrow().clone().unwrap();
                let id = doc
                    .nodes
                    .iter()
                    .position(|n| n.label.starts_with("auditData:"))
                    .unwrap();
                let range = &doc.nodes[id].chars;
                tab.select_range(range.start, range.end);
                gtk::prelude::WidgetExt::activate_action(&app.window, "win.decode", None).unwrap();
                stage.set(3);
            }
            3 if tab.text().contains("\"k\": 3") => {
                gtk::prelude::WidgetExt::activate_action(&app.window, "win.undo", None).unwrap();
                assert!(tab.text().contains(r#"\"k\":3"#));
                gtk::prelude::WidgetExt::activate_action(&app.window, "win.redo", None).unwrap();
                app.save_to(&tab, saved.clone());
                stage.set(4);
            }
            4 if !tab.saving.get() => {
                assert!(!tab.dirty.get());
                assert_eq!(std::fs::read_to_string(&saved).unwrap(), tab.text());
                std::fs::remove_file(&saved).unwrap();
                println!(
                    "UI_SMOKE_OK tabs search syntax ranges unicode lazy_groups debounce decode undo redo save"
                );
                app.closing.set(true);
                app.window.close();
                return glib::ControlFlow::Break;
            }
            _ => {}
        }
        glib::ControlFlow::Continue
    });
}
