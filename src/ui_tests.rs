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
fn opens_embedded_documents_without_changing_the_source() {
    for (source, outer, inner, expected) in [
        (
            r#"{"da:ta": "\u003croot\u003e\n<item/>\u003c/root\u003e"}"#,
            Kind::Json,
            Kind::Xml,
            "<root>\n<item/></root>",
        ),
        (
            r#"<data>{&quot;value&quot;:&quot;a &amp; b&quot;}</data>"#,
            Kind::Xml,
            Kind::Json,
            r#"{"value":"a & b"}"#,
        ),
        (
            r#"<data><![CDATA[["&amp;"]]]></data>"#,
            Kind::Xml,
            Kind::Json,
            r#"["&amp;"]"#,
        ),
    ] {
        let mut app = App::new(None);
        load(&mut app, 0, source, outer);
        let original = &app.tabs[0];
        app.perform(
            Action::OpenEmbedded {
                tab: original.id,
                node: 1,
            },
            &egui::Context::default(),
            None,
        );
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.active, 1);
        wait(&mut app, current);
        assert_eq!(app.tabs[1].kind, Some(inner));
        assert_eq!(app.tabs[1].editor.text(), expected);
        assert_eq!(app.tabs[0].editor.text(), source);
        assert!(!app.tabs[0].dirty);
        assert!(app.tabs[1].path.is_none());
    }
}

#[test]
fn embedded_open_skips_plain_text_and_stale_tree_nodes() {
    let mut app = App::new(None);
    load(&mut app, 0, r#"["Not an XML document"]"#, Kind::Json);
    let tab = app.tabs[0].id;
    app.open_embedded(tab, 1);
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs[0].status, "This node does not contain XML");

    load(&mut app, 0, r#"["<root/>"]"#, Kind::Json);
    app.tabs[0].editor.set_text("[]");
    app.open_embedded(tab, 1);
    assert_eq!(app.tabs.len(), 1);
    assert!(!app.tabs[0].tree_current());
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
        Output::Opened(Ok(set)) => {
            assert_eq!(set.documents.len(), 1);
            let document = &set.documents[0];
            assert_eq!(document.kind, Kind::Json);
            assert!(document.parsed.is_err());
            assert_eq!(document.text.as_deref(), Some("{\"a\":"));
        }
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
        Output::Opened(Err(_))
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
        Output::Opened(Ok(set)) => {
            assert_eq!(set.documents.len(), 1);
            let document = &set.documents[0];
            assert_eq!(document.kind, Kind::Json);
            let content = document.text.as_deref().expect("source text");
            assert_eq!(content, text);
            let document = document.parsed.as_ref().expect("document parses");
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
fn decode_selected_json_property_keeps_document_valid() {
    let ctx = egui::Context::default();
    let mut app = App::new(None);
    load(
        &mut app,
        0,
        r#"{"auditData":{"data":"{\"request\":{\"id\":1}}"}}"#,
        Kind::Json,
    );
    let id = app.tabs[0]
        .document
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .position(|node| node.label.starts_with("data:"))
        .unwrap();
    app.tabs[0].select_node(id);
    assert!(app.tabs[0].editor.selected_text().starts_with("\"data\":"));

    app.perform(Action::Decode, &ctx, None);
    wait(&mut app, |app| {
        app.tabs[0].editor.text().contains("\n  \"auditData\": {")
            && app.tabs[0].editor.text().contains("\"id\": 1")
    });
    let value: serde_json::Value = serde_json::from_str(&app.tabs[0].editor.text()).unwrap();
    assert_eq!(value["auditData"]["data"]["request"]["id"], 1);
}

#[test]
fn decode_without_selection_uses_the_whole_document() {
    let ctx = egui::Context::default();
    let mut app = App::new(None);
    load(
        &mut app,
        0,
        r#"{"auditData":{"data":"{\"request\":{\"id\":1}}"}}"#,
        Kind::Json,
    );
    app.tabs[0].editor.select_range(0, 0);

    app.perform(Action::Decode, &ctx, None);

    assert_ne!(app.tabs[0].status, "Select text first (Ctrl+E)");
    wait(&mut app, |app| {
        app.tabs[0].editor.text().contains("\n  \"auditData\": {")
            && app.tabs[0].editor.text().contains("\"id\": 1")
    });
    let value: serde_json::Value = serde_json::from_str(&app.tabs[0].editor.text()).unwrap();
    assert_eq!(value["auditData"]["data"]["request"]["id"], 1);
}

#[test]
fn decode_without_selection_structures_xml_document() {
    let ctx = egui::Context::default();
    let mut app = App::new(None);
    load(
        &mut app,
        0,
        "<auditData>&lt;r&gt;&lt;value&gt;1&lt;/value&gt;&lt;/r&gt;</auditData>",
        Kind::Xml,
    );
    app.tabs[0].editor.select_range(0, 0);

    app.perform(Action::Decode, &ctx, None);

    wait(&mut app, |app| {
        app.tabs[0].editor.text().contains("\n  <r>")
    });
    let text = app.tabs[0].editor.text();
    document::parse(Arc::from(text.as_str()), Kind::Xml, || false).unwrap();
    assert!(text.contains("<value>1</value>"));
}

#[test]
fn save_all_and_quit_saves_every_modified_tab_before_closing() {
    let ctx = egui::Context::default();
    let mut app = App::new(None);
    let paths: Vec<PathBuf> = (0..2)
        .map(|index| unique_path(&format!("xmljson-quit-{index}")).with_extension("json"))
        .collect();
    for (index, path) in paths.iter().enumerate() {
        if index > 0 {
            app.new_tab();
        }
        let tab = &mut app.tabs[index];
        tab.path = Some(path.clone());
        tab.editor.set_text(&format!("{{\"tab\":{index}}}"));
        tab.dirty = true;
    }
    app.save_all_and_quit(None);
    assert!(app.quit_after_save);
    app.quit_when_saved(&ctx);
    assert!(!app.closing, "waits for the saves to finish");
    wait(&mut app, |app| app.tabs.iter().all(|tab| !tab.saving));
    app.quit_when_saved(&ctx);
    assert!(app.closing);
    for (index, path) in paths.iter().enumerate() {
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            format!("{{\"tab\":{index}}}")
        );
        fs::remove_file(path).unwrap();
    }
}

#[test]
fn update_checks_offer_newer_versions_and_respect_skipped_ones() {
    let mut app = App::new(None);
    let check = |app: &mut App, version: &str, manual: bool| {
        let (send, receive) = mpsc::channel();
        app.update_replies = Some(receive);
        send.send(UpdateCheck {
            result: Ok(Release {
                version: version.into(),
            }),
            manual,
        })
        .unwrap();
        app.poll_update_check();
        assert!(app.update_replies.is_none());
    };
    check(&mut app, "999.0.0", false);
    assert_eq!(app.update_available.as_ref().unwrap().version, "999.0.0");

    app.update_available = None;
    app.skipped_version = "999.0.0".into();
    check(&mut app, "999.0.0", false);
    assert!(
        app.update_available.is_none(),
        "skipped versions stay quiet"
    );
    check(&mut app, "999.0.0", true);
    assert!(
        app.update_available.is_some(),
        "a manual check shows it again"
    );
    assert_eq!(app.tabs[0].status, "Version 999.0.0 is available");

    check(&mut app, CURRENT_VERSION, true);
    assert!(app.update_available.is_none());
    assert!(
        app.tabs[0]
            .status
            .starts_with("You have the latest version")
    );

    let (send, receive) = mpsc::channel();
    app.update_replies = Some(receive);
    send.send(UpdateCheck {
        result: Err("offline".into()),
        manual: false,
    })
    .unwrap();
    let status = app.tabs[0].status.clone();
    app.poll_update_check();
    assert_eq!(app.tabs[0].status, status, "automatic failures are silent");
}

#[test]
fn parse_errors_are_kept_for_the_tree_hint_until_the_text_parses() {
    let mut app = App::new(None);
    app.tabs[0].editor.set_text("log line {\"a\":");
    app.parse(0, Some(Kind::Json));
    wait(&mut app, |app| app.tabs[0].parse_count == 1);
    assert!(app.tabs[0].parse_error.is_some());
    load(&mut app, 0, r#"{"a":1}"#, Kind::Json);
    assert!(app.tabs[0].parse_error.is_none());
}

#[test]
fn decode_on_open_structures_the_file_without_marking_it_dirty() {
    let path = unique_path("xmljson-decode-on-open").with_extension("json");
    let original = r#"{"auditData":{"data":"{\"request\":{\"id\":1}}"}}"#;
    fs::write(&path, original).unwrap();
    let mut app = App::new(None);
    app.decode_on_open = true;
    app.open(path.clone());
    wait(&mut app, |app| {
        app.tabs[0].editor.text().contains("\"id\": 1")
    });
    app.process_changes(Instant::now());
    assert!(!app.tabs[0].dirty, "the file on disk is unchanged");
    assert!(app.tabs[0].status.starts_with("Decoded on open"));
    assert!(app.tabs[0].editor.undo());
    assert_eq!(app.tabs[0].editor.text(), original);
    fs::remove_file(path).unwrap();
}

#[test]
fn files_forwarded_from_another_instance_open_as_tabs() {
    let directory = unique_path("xmljson-single-instance");
    fs::create_dir(&directory).unwrap();
    let json_path = directory.join("first.json");
    let xml_path = directory.join("second.xml");
    fs::write(&json_path, r#"{"first":1}"#).unwrap();
    fs::write(&xml_path, "<second/>").unwrap();

    let ctx = egui::Context::default();
    let mut app = App::new(None);
    let (send, requests) = mpsc::channel();
    app.open_requests = Some(requests);
    send.send(vec![json_path.clone(), xml_path.clone()])
        .unwrap();
    app.receive_open_requests(&ctx);

    assert_eq!(app.tabs.len(), 2);
    wait(&mut app, |app| {
        app.tabs
            .iter()
            .all(|tab| tab.path.is_some() && tab.editor.editable)
    });
    assert_eq!(app.tabs[0].path.as_ref(), Some(&json_path));
    assert_eq!(app.tabs[1].path.as_ref(), Some(&xml_path));

    fs::remove_file(json_path).unwrap();
    fs::remove_file(xml_path).unwrap();
    fs::remove_dir(directory).unwrap();
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
fn replace_respects_case_option_and_marks_the_tab_dirty() {
    let mut app = App::new(None);
    load(&mut app, 0, r#"{"Name":"a","name":"b"}"#, Kind::Json);
    app.search_query = "NAME".into();
    app.replace_text = "id".into();
    app.replace(true);
    assert_eq!(app.tabs[0].status, "No matches found");
    app.search_case_sensitive = false;
    app.replace(true);
    assert_eq!(app.tabs[0].editor.text(), r#"{"id":"a","id":"b"}"#);
    assert_eq!(app.tabs[0].status, "Replaced 2 occurrence(s)");
    app.process_changes(Instant::now());
    assert!(app.tabs[0].dirty);
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
    assert_eq!(app.title, "XML/JSON Viewer — Untitled");
    assert!(app.tabs[0].editor.match_count() > 1100);

    let search_rect = || ctx.read_response(Id::unique(SEARCH_ID)).unwrap().rect;
    let before = search_rect();
    app.replace_visible = true;
    for _ in 0..3 {
        frame(&mut app);
    }
    let replace = ctx.read_response(Id::unique(REPLACE_ID)).unwrap().rect;
    let after = search_rect();
    frame(&mut app);
    assert_eq!(search_rect(), after, "the panel size is stable");
    assert!(
        after.top() < before.top() - replace.height() / 2.0,
        "the search panel grows to fit the replace row"
    );
    assert!(replace.top() >= after.bottom());
}

#[test]
fn startup_plan_orders_dx12_glow_and_software_fallbacks() {
    assert_eq!(
        startup_plan(RendererChoice::Auto, None, None, true),
        vec![
            StartupAttempt::WgpuDx12,
            StartupAttempt::Glow,
            StartupAttempt::WgpuSoftware,
        ]
    );
    assert_eq!(
        startup_plan(RendererChoice::Auto, Some("wgpu-dx12"), None, true),
        vec![
            StartupAttempt::Glow,
            StartupAttempt::WgpuSoftware,
            StartupAttempt::WgpuDx12,
        ]
    );
    assert_eq!(
        startup_plan(RendererChoice::Auto, Some("glow"), None, true),
        vec![
            StartupAttempt::WgpuSoftware,
            StartupAttempt::WgpuDx12,
            StartupAttempt::Glow,
        ]
    );
    assert_eq!(
        startup_plan(RendererChoice::Auto, None, None, false),
        vec![StartupAttempt::WgpuDefault, StartupAttempt::Glow]
    );
    assert_eq!(
        startup_plan(RendererChoice::Glow, None, None, true),
        vec![StartupAttempt::Glow]
    );
    assert_eq!(
        startup_plan(RendererChoice::WgpuSoftware, None, None, true),
        vec![StartupAttempt::WgpuSoftware]
    );
}

#[test]
fn startup_plan_keeps_last_working_backend_after_a_crash_fallback() {
    assert_eq!(
        startup_plan(RendererChoice::Auto, None, Some("glow"), true),
        vec![
            StartupAttempt::Glow,
            StartupAttempt::WgpuDx12,
            StartupAttempt::WgpuSoftware,
        ]
    );
    assert_eq!(
        startup_plan(RendererChoice::Auto, None, Some("wgpu-software"), true),
        vec![
            StartupAttempt::WgpuSoftware,
            StartupAttempt::WgpuDx12,
            StartupAttempt::Glow,
        ]
    );
    assert_eq!(
        startup_plan(RendererChoice::Auto, Some("glow"), Some("glow"), true),
        vec![
            StartupAttempt::WgpuSoftware,
            StartupAttempt::WgpuDx12,
            StartupAttempt::Glow,
        ]
    );
    assert_eq!(
        startup_plan(RendererChoice::Auto, None, Some("unknown"), true),
        vec![
            StartupAttempt::WgpuDx12,
            StartupAttempt::Glow,
            StartupAttempt::WgpuSoftware,
        ]
    );
    assert_eq!(
        startup_plan(RendererChoice::WgpuDx12, None, Some("glow"), true),
        vec![StartupAttempt::WgpuDx12]
    );
}

fn write_zip(path: &Path, files: &[(&str, &[u8])]) {
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    let file = fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    for (name, bytes) in files {
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file(*name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
}

#[test]
fn opening_a_zip_creates_a_tab_per_text_file() {
    let directory = unique_path("xmljson-zip");
    fs::create_dir(&directory).unwrap();
    let path = directory.join("sample.zip");
    write_zip(
        &path,
        &[
            ("nested/a.xml", b"<root/>"),
            ("b.json", br#"{"n":1}"#),
            ("bad.bin", &[0, 255, 254]),
        ],
    );

    let mut app = App::new(None);
    app.open(path.clone());
    wait(&mut app, |app| matches!(app.dialog, Some(Dialog::Zip(_))));
    match &app.dialog {
        Some(Dialog::Zip(prompt)) => {
            assert_eq!(prompt.names, ["nested/a.xml", "b.json"]);
            assert!(prompt.skipped > 0);
        }
        _ => panic!("zip should ask before opening"),
    }
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs[0].editor.len_chars(), 0);
    app.dialog = None;
    assert_eq!(app.tabs.len(), 1);

    app.open(path);
    wait(&mut app, |app| matches!(app.dialog, Some(Dialog::Zip(_))));
    if let Some(Dialog::Zip(prompt)) = &mut app.dialog {
        prompt.selected[1] = true;
    }
    app.open_confirmed_zip(false);
    wait(&mut app, |app| {
        app.tabs.len() == 1 && app.tabs[0].editor.editable && app.tabs[0].tree_current()
    });
    assert_eq!(app.tabs[0].title(), "b.json");
    assert!(app.tabs[0].path.is_none());

    let mut app = App::new(None);
    app.open(directory.join("sample.zip"));
    wait(&mut app, |app| matches!(app.dialog, Some(Dialog::Zip(_))));
    app.open_confirmed_zip(true);
    wait(&mut app, |app| {
        app.tabs.len() == 2
            && app
                .tabs
                .iter()
                .all(|tab| tab.editor.editable && tab.tree_current())
    });
    let titles: Vec<_> = app.tabs.iter().map(|tab| tab.title()).collect();
    assert!(titles.iter().any(|title| title == "nested/a.xml"));
    assert!(titles.iter().any(|title| title == "b.json"));
    assert!(app.tabs.iter().all(|tab| tab.path.is_none() && !tab.dirty));
    assert!(app.tabs.iter().any(|tab| tab.status.contains("Skipped")));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn decode_on_open_applies_to_files_inside_a_zip() {
    let directory = unique_path("xmljson-zip-decode");
    fs::create_dir(&directory).unwrap();
    let path = directory.join("sample.zip");
    let original = r#"{"auditData":{"data":"{\"request\":{\"id\":1}}"}}"#;
    write_zip(&path, &[("wrapped.json", original.as_bytes())]);

    let mut app = App::new(None);
    app.decode_on_open = true;
    app.open(path);
    wait(&mut app, |app| matches!(app.dialog, Some(Dialog::Zip(_))));
    app.open_confirmed_zip(true);
    wait(&mut app, |app| {
        app.tabs[0].editor.text().contains("\"id\": 1")
    });
    assert!(
        !app.tabs[0].dirty,
        "the zip on disk is unchanged, as with regular files"
    );
    assert!(app.tabs[0].path.is_none());
    assert_eq!(app.tabs[0].title(), "wrapped.json");
    assert!(app.tabs[0].editor.undo());
    assert_eq!(app.tabs[0].editor.text(), original);
    fs::remove_dir_all(directory).unwrap();
}
