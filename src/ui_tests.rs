use super::*;

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

#[test]
fn worker_reports_io_errors_and_preserves_invalid_source() {
    let directory = unique_path("xmljson-worker");
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("invalid.json");
    std::fs::write(&path, "{\"a\":").unwrap();
    let (send, receive) = worker();
    let revision = Arc::new(AtomicU64::new(0));
    send.send(Job {
        tab: 1,
        revision: 0,
        current: revision.clone(),
        task: Task::Open(path.clone()),
    })
    .unwrap();
    match receive.recv_timeout(Duration::from_secs(5)).unwrap().output {
        Output::Opened(_, Kind::Json, Err(_), Some(content), Some(_), false) => {
            assert_eq!(content, "{\"a\":")
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
        Output::Opened(_, _, Err(_), None, None, false)
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
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

#[test]
fn worker_cancels_obsolete_parse_but_keeps_requested_save_snapshot() {
    let path = unique_path("xmljson-save");
    let (send, receive) = worker();
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
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[1]");
    std::fs::remove_file(path).unwrap();
}

#[test]
fn worker_adapts_extreme_lines_without_truncating_or_writing_source() {
    let directory = unique_path("xmljson-long");
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("long.json");
    let text = format!(r#"{{"same":1,"same":2,"data":"{}"}}"#, "é😀".repeat(20_000));
    std::fs::write(&path, &text).unwrap();
    let (send, receive) = worker();
    send.send(Job {
        tab: 1,
        revision: 0,
        current: Arc::new(AtomicU64::new(0)),
        task: Task::Open(path.clone()),
    })
    .unwrap();
    match receive.recv_timeout(Duration::from_secs(5)).unwrap().output {
        Output::Opened(_, Kind::Json, Ok(document), Some(content), Some(highlight), true) => {
            assert!(content.contains("\n"));
            assert!(content.contains(&"é😀".repeat(20_000)));
            assert_eq!(
                document
                    .nodes
                    .iter()
                    .filter(|node| node.label.starts_with("same:"))
                    .count(),
                2
            );
            assert_eq!(highlight.long_lines.len(), 1);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
        _ => panic!("Extreme line must be adapted with complete source and syntax"),
    }
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}
