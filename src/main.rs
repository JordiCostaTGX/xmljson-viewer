#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod diagnostics;
mod document;
mod editor;
mod settings;
mod single_instance;
mod syntax;
mod theme;
mod tree;
mod tree_style;
mod ui;

use document::Kind;
use single_instance::Launch;
use std::{ffi::OsString, path::PathBuf, sync::Arc, time::Instant};
use ui::RendererChoice;

const NEW_WINDOW_FLAG: &str = "--new-window";

struct LaunchArgs {
    renderer: RendererChoice,
    new_window: bool,
    files: Vec<PathBuf>,
}

fn parse_launch_args(args: Vec<OsString>) -> LaunchArgs {
    let mut choice = std::env::var("XMLJSON_RENDERER")
        .ok()
        .and_then(|value| RendererChoice::parse(&value))
        .unwrap_or(RendererChoice::Auto);
    let mut new_window = false;
    let mut files = Vec::new();
    let mut literal_only = false;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        if !literal_only && let Some(text) = arg.to_str() {
            if text == "--" {
                literal_only = true;
                continue;
            }
            if let Some(value) = text.strip_prefix("--renderer=")
                && let Some(parsed) = RendererChoice::parse(value)
            {
                choice = parsed;
                continue;
            }
            if text == "--renderer" {
                if let Some(next) = iter.next() {
                    if let Some(parsed) = next.to_str().and_then(RendererChoice::parse) {
                        choice = parsed;
                    } else {
                        files.push(PathBuf::from(next));
                    }
                }
                continue;
            }
            match text {
                NEW_WINDOW_FLAG => {
                    new_window = true;
                    continue;
                }
                "--gl" | "--glow" => {
                    choice = RendererChoice::Glow;
                    continue;
                }
                "--dx12" => {
                    choice = RendererChoice::WgpuDx12;
                    continue;
                }
                "--wgpu" => {
                    choice = RendererChoice::Wgpu;
                    continue;
                }
                "--software" => {
                    choice = RendererChoice::WgpuSoftware;
                    continue;
                }
                _ => {}
            }
        }
        files.push(PathBuf::from(arg));
    }
    LaunchArgs {
        renderer: choice,
        new_window,
        files,
    }
}

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
    let LaunchArgs {
        renderer: renderer_choice,
        new_window,
        files,
    } = parse_launch_args(args);
    // Launching without files (e.g. middle-click on the taskbar icon) asks for another window.
    let new_window = new_window || files.is_empty() || settings::load().open_files_in_new_window;
    diagnostics::record(&format!(
        "Open requests: {} file(s); renderer preference: {renderer_choice:?}; new window: {new_window}",
        files.len()
    ));
    let (incoming, requests) = std::sync::mpsc::channel();
    diagnostics::record("Initializing single-instance listener or forwarding request");
    match single_instance::forward_or_start(files.clone(), incoming, new_window) {
        Ok(Launch::Forwarded) => {
            diagnostics::record("Request forwarded to existing instance; exiting");
            return;
        }
        Ok(Launch::Primary) => diagnostics::record("Single-instance listener ready"),
        Ok(Launch::Independent) => {
            diagnostics::record("Another window owns the listener; starting an independent window")
        }
        Err(error) => {
            diagnostics::report_error(&format!("Single-instance initialization failed: {error}"));
            std::process::exit(1);
        }
    }
    diagnostics::record("Initializing window and graphics renderer");
    if let Err(error) = ui::run(files, requests, renderer_choice) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_launch_args_extracts_renderer_flags_and_keeps_files() {
        let args = parse_launch_args(vec![
            OsString::from("--gl"),
            OsString::from("sample.json"),
            OsString::from("--"),
            OsString::from("--dx12"),
        ]);
        assert_eq!(args.renderer, RendererChoice::Glow);
        assert!(!args.new_window);
        assert_eq!(
            args.files,
            vec![PathBuf::from("sample.json"), PathBuf::from("--dx12")]
        );

        let args = parse_launch_args(vec![
            OsString::from("--renderer=software"),
            OsString::from(NEW_WINDOW_FLAG),
            OsString::from("audit.xml"),
        ]);
        assert_eq!(args.renderer, RendererChoice::WgpuSoftware);
        assert!(args.new_window);
        assert_eq!(args.files, vec![PathBuf::from("audit.xml")]);
    }
}
