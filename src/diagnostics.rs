use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

static LOG: OnceLock<StartupLog> = OnceLock::new();
static LOGGER: DiagnosticsLogger = DiagnosticsLogger;
const MAX_LOG_BYTES: u64 = 1024 * 1024;

struct DiagnosticsLogger;

impl log::Log for DiagnosticsLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        match metadata.level() {
            log::Level::Error | log::Level::Warn => true,
            log::Level::Info => {
                let target = metadata.target();
                target.starts_with("eframe")
                    || target.starts_with("egui_wgpu")
                    || target.starts_with("egui_glow")
                    || target.starts_with("wgpu")
                    || target.starts_with("winit")
            }
            _ => false,
        }
    }

    fn log(&self, record_entry: &log::Record<'_>) {
        if self.enabled(record_entry.metadata()) {
            record(&format!(
                "[{} {}] {}",
                record_entry.level(),
                record_entry.target(),
                record_entry.args()
            ));
        }
    }

    fn flush(&self) {}
}

struct StartupLog {
    directory: PathBuf,
    path: PathBuf,
    file: Mutex<File>,
}

impl StartupLog {
    fn create(directory: &Path) -> io::Result<Self> {
        fs::create_dir_all(directory)?;
        let path = directory.join("startup.log");
        // Several windows share the log, so it is only rotated when it grows large.
        if fs::metadata(&path).is_ok_and(|metadata| metadata.len() > MAX_LOG_BYTES) {
            let _ = fs::rename(&path, directory.join("startup.old.log"));
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            directory: directory.to_path_buf(),
            path,
            file: Mutex::new(file),
        })
    }

    fn pending_marker_path(&self) -> PathBuf {
        self.directory.join("graphics-init.pending")
    }

    fn remembered_renderer_path(&self) -> PathBuf {
        self.directory.join("graphics-renderer")
    }

    fn write(&self, message: &str) {
        if let Ok(mut file) = self.file.lock() {
            write_line(&mut file, message);
        }
    }

    // A crashing thread may already hold the lock; blocking would hang instead of exiting.
    #[cfg(target_os = "windows")]
    fn try_write(&self, message: &str) {
        if let Ok(mut file) = self.file.try_lock() {
            write_line(&mut file, message);
        }
    }
}

fn write_line(file: &mut File, message: &str) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let _ = writeln!(
        file,
        "[{}] [pid {}] {message}",
        timestamp.as_secs_f64(),
        std::process::id()
    );
    let _ = file.flush();
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn unhandled_exception_filter(
    info: *const windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
) -> i32 {
    use windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_CONTINUE_SEARCH;

    if !info.is_null() {
        let record_ptr = unsafe { (*info).ExceptionRecord };
        if !record_ptr.is_null() {
            let code = unsafe { (*record_ptr).ExceptionCode as u32 };
            let address = unsafe { (*record_ptr).ExceptionAddress as usize };
            let label = match code {
                0xC000_0005 => "STATUS_ACCESS_VIOLATION",
                0xC000_001D => "STATUS_ILLEGAL_INSTRUCTION",
                0xC000_00FD => "STATUS_STACK_OVERFLOW",
                0xC000_0409 => "STATUS_STACK_BUFFER_OVERRUN",
                0x8000_0003 => "STATUS_BREAKPOINT",
                _ => "UNKNOWN_EXCEPTION",
            };
            record_without_blocking(&format!(
                "FATAL: Unhandled Windows exception 0x{code:08X} ({label}) at 0x{address:016X}"
            ));
        } else {
            record_without_blocking("FATAL: Unhandled Windows exception (no exception record)");
        }
    } else {
        record_without_blocking("FATAL: Unhandled Windows exception (null pointers)");
    }
    EXCEPTION_CONTINUE_SEARCH
}

#[cfg(target_os = "windows")]
fn record_without_blocking(message: &str) {
    if let Some(log) = LOG.get() {
        log.try_write(message);
    }
}

#[cfg(target_os = "windows")]
fn install_native_crash_handler() {
    use windows_sys::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter;
    unsafe {
        SetUnhandledExceptionFilter(Some(unhandled_exception_filter));
    }
}

pub fn initialize() {
    let temporary = std::env::temp_dir().join("xmljson-viewer").join("logs");
    let preferred = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("xmljson-viewer").join("logs"))
        .unwrap_or_else(|| temporary.clone());
    match StartupLog::create(&preferred).or_else(|_| StartupLog::create(&temporary)) {
        Ok(log) => {
            let _ = LOG.set(log);
        }
        Err(error) => eprintln!("Could not create startup log: {error}"),
    }
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    #[cfg(target_os = "windows")]
    install_native_crash_handler();
    std::panic::set_hook(Box::new(|info| {
        let message = format!("Unexpected panic: {info}");
        record(&format!(
            "{message}\nBacktrace:\n{}",
            std::backtrace::Backtrace::force_capture()
        ));
        show_error(&message);
    }));
    record(&format!(
        "Starting XML/JSON Viewer {} on {}/{}; pid={}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::process::id()
    ));
}

pub fn previous_graphics_init_failure() -> Option<String> {
    let path = LOG.get()?.pending_marker_path();
    let content = fs::read_to_string(path).ok()?;
    let trimmed = content.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub fn mark_graphics_init_started(attempt: &str) {
    if let Some(log) = LOG.get() {
        let _ = fs::write(log.pending_marker_path(), attempt);
    }
}

pub fn mark_graphics_init_succeeded() {
    if let Some(log) = LOG.get() {
        let _ = fs::remove_file(log.pending_marker_path());
    }
}

pub fn remembered_renderer() -> Option<String> {
    let content = fs::read_to_string(LOG.get()?.remembered_renderer_path()).ok()?;
    let trimmed = content.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub fn remember_renderer(attempt: &str) {
    if let Some(log) = LOG.get() {
        let _ = fs::write(log.remembered_renderer_path(), attempt);
    }
}

pub fn record(message: &str) {
    if let Some(log) = LOG.get() {
        log.write(message);
    }
}

pub fn report_error(message: &str) {
    record(&format!("ERROR: {message}"));
    show_error(message);
}

fn show_error(message: &str) {
    eprintln!("{message}");
    let location = match LOG.get() {
        Some(log) => {
            let status = match log_viewer_command(&log.path).spawn() {
                Ok(_) => "The diagnostic log has been opened.".into(),
                Err(error) => {
                    record(&format!("Could not open diagnostic log: {error}"));
                    format!("Could not open the diagnostic log automatically: {error}")
                }
            };
            format!("{status}\n\nDiagnostic log:\n{}", log.path.display())
        }
        None => "The diagnostic log could not be created.".into(),
    };
    rfd::MessageDialog::new()
        .set_title("XML/JSON Viewer - Error")
        .set_description(format!("{message}\n\n{location}"))
        .set_level(rfd::MessageLevel::Error)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

fn log_viewer_command(path: &Path) -> std::process::Command {
    #[cfg(target_os = "windows")]
    let program = "notepad.exe";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let program = "xdg-open";
    let mut command = std::process::Command::new(program);
    command.arg(path);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_log_keeps_entries_from_every_process_and_rotates_when_large() {
        let directory = std::env::temp_dir().join(format!(
            "xmljson-viewer-diagnostics-test-{}",
            std::process::id()
        ));
        let first = StartupLog::create(&directory).unwrap();
        first.write("Starting viewer");
        first.write("ERROR: Could not start listener");
        let text = fs::read_to_string(&first.path).unwrap();
        assert!(text.contains("Starting viewer"));
        assert!(text.contains(&format!("[pid {}]", std::process::id())));
        let second = StartupLog::create(&directory).unwrap();
        assert_eq!(first.path, second.path);
        assert_eq!(second.path.file_name().unwrap(), "startup.log");
        second.write("New launch");
        first.write("Existing instance still running");
        let text = fs::read_to_string(&second.path).unwrap();
        assert!(text.contains("Could not start listener"));
        assert!(text.contains("New launch"));
        assert!(text.contains("Existing instance still running"));
        assert!(!text.contains('\0'));
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        drop(first);
        drop(second);
        fs::write(
            directory.join("startup.log"),
            vec![b'x'; MAX_LOG_BYTES as usize + 1],
        )
        .unwrap();
        let rotated = StartupLog::create(&directory).unwrap();
        rotated.write("After rotation");
        let text = fs::read_to_string(&rotated.path).unwrap();
        assert!(text.contains("After rotation"));
        assert!(!text.contains("xxx"));
        assert!(directory.join("startup.old.log").exists());
        drop(rotated);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn startup_log_returns_error_for_unwritable_location() {
        let directory = std::env::temp_dir().join(format!(
            "xmljson-viewer-diagnostics-invalid-test-{}",
            std::process::id()
        ));
        let log = StartupLog::create(&directory).unwrap();
        assert!(StartupLog::create(&log.path.join("logs")).is_err());
        drop(log);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn log_viewer_receives_path_as_one_literal_argument() {
        let path = Path::new("diagnostic logs/startup.log");
        let command = log_viewer_command(path);
        assert_eq!(command.get_args().collect::<Vec<_>>(), [path.as_os_str()]);
        #[cfg(target_os = "windows")]
        assert_eq!(command.get_program(), "notepad.exe");
    }
}
