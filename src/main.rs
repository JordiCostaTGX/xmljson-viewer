mod document;
mod syntax;
mod tree;
mod tree_style;
mod ui;

use document::Kind;
use std::{sync::Arc, time::Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|arg| arg == "--benchmark") {
        if let Err(error) = benchmark(&args[2..]) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    ui::run();
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
        peak_rss_kib()?
    );
    Ok(())
}

fn peak_rss_kib() -> Result<u64, String> {
    let status = std::fs::read_to_string("/proc/self/status").map_err(|e| e.to_string())?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .ok_or_else(|| "Cannot measure VmHWM".to_string())?
        .split_whitespace()
        .next()
        .ok_or("Empty VmHWM")?
        .parse()
        .map_err(|e| format!("Invalid VmHWM: {e}"))
}
