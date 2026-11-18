use super::*;
use eframe::egui::{Pos2, RawInput, Rect, vec2};

fn unique_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn wait(app: &mut App, done: impl Fn(&App) -> bool) {
    let start = Instant::now();
    while !done(app) {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "Timeout: {}",
            app.tabs[app.active].status
        );
        if let Ok(reply) = app.replies.recv_timeout(Duration::from_millis(50)) {
            app.accept(reply);
        }
    }
}

fn current(app: &App) -> bool {
    app.tabs[app.active].tree_current()
}

fn load(app: &mut App, index: usize, text: &str, kind: Kind) {
    app.active = index;
    let tab = &mut app.tabs[index];
    tab.editor.set_text(text);
    tab.seen_version = tab.editor.version();
    app.parse(index, Some(kind));
    wait(app, current);
}

#[test]
fn worker_reports_io_errors_and_preserves_invalid_source() {
    let directory = unique_path("xmljson-worker");
    fs::create_dir(&directory).unwrap();
    let path = directory.join("invalid.json");
    fs::write(&path, "{\"a\":").unwrap();
    let (send, receive) = worker(|| {});
    let revision = Arc::new(AtomicU64::new(0));
    send.send(Job {
        tab: 1,
        revision: 0,
        current: revision.clone(),
        task: Task::Open(path.clone()),
    })
    .unwrap();
    match receive.recv_timeout(Duration::from_secs(5)).unwrap().output {
        Output::Opened(_, Kind::Json, Err(_), Some(content)) => assert_eq!(&*content, "{\"a\":"),
        _ => panic!("Invalid source must remain available for editing"),
    }
    send.send(Job {
        tab: 1,
        revision: 0,
        current: revision.clone(),
        task: Task::Open(directory.join("missing.json")),
    })
    .unwrap();
    assert!(matches!(
        receive.recv_timeout(Duration::from_secs(5)).unwrap().output,
        Output::Opened(_, _, Err(_), None)
    ));
    send.send(Job {
        tab: 1,
        revision: 0,
        current: revision,
        task: Task::Save(directory.clone(), "text".into()),
    })
    .unwrap();
    assert!(matches!(
        receive.recv_timeout(Duration::from_secs(5)).unwrap().output,
        Output::Saved(_, Err(_))
    ));
    fs::remove_file(path).unwrap();
    fs::remove_dir(directory).unwrap();
}

#[test]
fn worker_cancels_obsolete_parse_but_keeps_requested_save_snapshot() {
    let path = unique_path("xmljson-save");
    let (send, receive) = worker(|| {});
    let revision = Arc::new(AtomicU64::new(1));
    send.send(Job {
        tab: 1,
        revision: 0,
        current: revision.clone(),
        task: Task::Parse(Arc::from("[]"), Kind::Json),
    })
    .unwrap();
    send.send(Job {
        tab: 1,
        revision: 0,
        current: revision,
        task: Task::Save(path.clone(), "[1]".into()),
    })
    .unwrap();
    assert!(matches!(
        receive.recv_timeout(Duration::from_secs(5)).unwrap().output,
        Output::Saved(_, Ok(()))
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), "[1]");
    write_atomic(&path, "[2]").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "[2]");
    fs::remove_file(path).unwrap();
}

#[test]
fn worker_opens_extreme_lines_without_changing_the_source() {
    let directory = unique_path("xmljson-long");
    fs::create_dir(&directory).unwrap();
    let path = directory.join("long.json");
    let text = format!(r#"{{"same":1,"same":2,"data":"{}"}}"#, "é😀".repeat(20_000));
    fs::write(&path, &text).unwrap();
    let (send, receive) = worker(|| {});
    send.send(Job {
        tab: 1,
        revision: 0,
        current: Arc::new(AtomicU64::new(0)),
        task: Task::Open(path.clone()),
    })
    .unwrap();
    match receive.recv_timeout(Duration::from_secs(5)).unwrap().output {
        Output::Opened(_, Kind::Json, Ok(document), Some(content)) => {
            assert_eq!(&*content, text);
            assert_eq!(
                document
                    .nodes
                    .iter()
                    .filter(|node| node.label.starts_with("same:"))
                    .count(),
                2
            );
        }
        _ => panic!("Extreme line must open with complete source"),
    }
    fs::remove_file(path).unwrap();
    fs::remove_dir(directory).unwrap();
}

#[test]
fn smoke_ranges_tabs_groups_debounce_decode_undo_and_save() {
    let ctx = egui::Context::default();
    let mut app = App::new(None);
    load(
        &mut app,
        0,
        r#"{"é":"😀","a":[{"x":1},{"x":2}],"auditData":"{\"k\":3}"}"#,
        Kind::Json,
    );
    let tab = &mut app.tabs[0];
    let document = tab.document.clone().unwrap();
    let id = document
        .nodes
        .iter()
        .position(|n| n.label == "x: 2")
        .unwrap();
    tab.tree.reveal(id, false).unwrap();
    tab.select_node(id);
    assert_eq!(tab.editor.selected_text(), r#""x":2"#);
    app.search_query = "x".into();
    app.find(false);
    assert_eq!(app.tabs[0].editor.selected_text(), "x");

    let second = app.new_tab();
    load(&mut app, second, "<r><a/><a>text</a></r>", Kind::Xml);
    app.tabs[second].editor.place_cursor(6);
    app.select_at_cursor(false);
    assert_eq!(app.tabs[second].editor.selected_text(), "<a/>");
    assert_eq!(app.tabs.len(), 2);

    app.active = 0;
    let before = app.tabs[0].parse_count;
    let now = Instant::now();
    for _ in 0..100 {
        let editor = &mut app.tabs[0].editor;
        let end = editor.len_chars();
        editor.replace(end..end, " ");
        assert_eq!(app.process_changes(now), Some(DEBOUNCE));
    }
    assert_eq!(
        app.tabs[0].parse_count, before,
        "no parsing during keystrokes"
    );
    assert!(!app.tabs[0].tree_current());
    app.process_changes(now + Duration::from_secs(1));
    wait(&mut app, current);
    assert_eq!(
        app.tabs[0].parse_count, 2,
        "100 edits coalesced to one parse"
    );

    let tab = &mut app.tabs[0];
    let document = tab.document.clone().unwrap();
    let id = document
        .nodes
        .iter()
        .position(|n| n.label.starts_with("auditData:"))
        .unwrap();
    tab.select_node(id);
    app.perform(Action::Decode, &ctx, None);
    wait(&mut app, |app| {
        app.tabs[0].editor.text().contains("\"k\": 3")
    });
    app.perform(Action::Undo, &ctx, None);
    assert!(app.tabs[0].editor.text().contains(r#"\"k\":3"#));
    app.perform(Action::Redo, &ctx, None);
    assert!(app.tabs[0].editor.text().contains("\"k\": 3"));

    let saved = unique_path("xmljson-smoke").with_extension("json");
    app.save_to(0, saved.clone());
    wait(&mut app, |app| !app.tabs[0].saving);
    assert!(!app.tabs[0].dirty);
    assert_eq!(
        fs::read_to_string(&saved).unwrap(),
        app.tabs[0].editor.text()
    );
    assert_eq!(
        app.tabs[0].title(),
        saved.file_name().unwrap().to_string_lossy()
    );
    fs::remove_file(&saved).unwrap();
}

#[test]
fn dirty_tabs_ask_before_closing_and_clean_tabs_close_directly() {
    let mut app = App::new(None);
    app.new_tab();
    let id = app.tabs[1].id;
    app.close_tab(id);
    assert_eq!(app.tabs.len(), 1);
    app.tabs[0].editor.replace(0..0, "{}");
    app.process_changes(Instant::now());
    app.close_tab(app.tabs[0].id);
    assert!(matches!(app.dialog, Some(Dialog::CloseTab(_))));
    app.remove_tab(0);
    assert_eq!(app.tabs.len(), 1, "a fresh tab replaces the last one");
}

#[test]
fn renders_frames_headless() {
    let ctx = egui::Context::default();
    let mut app = App::new(Some(ctx.clone()));
    let mut text = String::from("<r>\n<!-- comment -->\n");
    for id in 0..1100 {
        text.push_str(&format!("  <e id=\"{id}\">&amp;</e>\n"));
    }
    text.push_str(&format!("<long>{}</long>\n</r>", "é".repeat(20_000)));
    load(&mut app, 0, &text, Kind::Xml);
    app.search_visible = true;
    app.search_query = "e".into();
    let input = || RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 800.0))),
        ..Default::default()
    };
    let frame = |app: &mut App| {
        ctx.run_ui(input(), |ui| app.show(ui, None))
            .textures_delta
            .clear();
    };
    for _ in 0..3 {
        frame(&mut app);
    }
    let end = app.tabs[0].editor.len_chars();
    app.tabs[0].editor.select_range(end - 10, end);
    frame(&mut app);
    assert_eq!(app.title, "XML/JSON Viewer Rust — Untitled");
    assert!(app.tabs[0].editor.match_count() > 1100);
}
