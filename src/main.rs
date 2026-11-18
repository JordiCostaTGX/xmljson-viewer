#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod document;
mod editor;
mod syntax;
mod tree;
mod tree_style;
mod ui;

use document::Kind;
use std::{path::PathBuf, sync::Arc, time::Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|arg| arg == "--benchmark") {
        if let Err(error) = benchmark(&args[2..]) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    let files = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if let Err(error) = ui::run(files) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn benchmark(args: &[String]) -> Result<(), String> {
    let path = args.first().ok_or("Usage: --benchmark FILE [ITERATIONS]")?;
    let text: Arc<str> = std::fs::read_to_string(path)
        .map_err(|e| e.to_string())?
        .into();
    let iterations: usize = args
        .get(1)
        .map(|s| s.parse())
        .transpose()
        .map_err(|_| "Invalid iteration count")?
        .unwrap_or(5);
    if iterations == 0 {
        return Err("At least one iteration is required".into());
    }
    let start = Instant::now();
    let mut nodes = 0;
    for _ in 0..iterations {
        nodes = std::hint::black_box(document::parse(text.clone(), Kind::detect(&text), || {
            false
        })?)
        .nodes
        .len();
    }
    println!(
        "bytes={} nodes={} iterations={} mean_ms={:.3} peak_rss_kib={}",
        text.len(),
        nodes,
        iterations,
        start.elapsed().as_secs_f64() * 1000.0 / iterations as f64,
        peak_rss_kib().map_or("n/a".into(), |kib| kib.to_string())
    );
    Ok(())
}

/// Linux only; other platforms report "n/a".
fn peak_rss_kib() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}
