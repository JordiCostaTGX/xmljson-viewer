#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod diagnostics;
mod document;
mod editor;
mod single_instance;
mod syntax;
mod tree;
mod tree_style;
mod ui;

use document::Kind;
use std::{path::PathBuf, sync::Arc, time::Instant};

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--benchmark") {
        let benchmark_args: Vec<_> = args[1..]
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        if let Err(error) = benchmark(&benchmark_args) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    diagnostics::initialize();
    let files: Vec<PathBuf> = args.into_iter().map(PathBuf::from).collect();
    diagnostics::record(&format!("Open requests: {} file(s)", files.len()));
    let (incoming, requests) = std::sync::mpsc::channel();
    diagnostics::record("Initializing single-instance listener or forwarding request");
    match single_instance::forward_or_start(files.clone(), incoming) {
        Ok(true) => {
            diagnostics::record("Request forwarded to existing instance; exiting");
            return;
        }
        Ok(false) => diagnostics::record("Single-instance listener ready"),
        Err(error) => {
            diagnostics::report_error(&format!("Single-instance initialization failed: {error}"));
            std::process::exit(1);
        }
    }
    diagnostics::record("Initializing window and graphics renderer");
    if let Err(error) = ui::run(files, requests) {
        diagnostics::report_error(&format!(
            "Window or graphics initialization failed: {error}"
        ));
        std::process::exit(1);
    }
    diagnostics::record("Application exited normally");
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
