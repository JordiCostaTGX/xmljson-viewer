use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

static LOG: OnceLock<StartupLog> = OnceLock::new();

struct StartupLog {
    path: PathBuf,
    file: Mutex<File>,
}

impl StartupLog {
    fn create(directory: &Path) -> io::Result<Self> {
        fs::create_dir_all(directory)?;
        let path = directory.join("startup.log");
        drop(File::create(&path)?);
        let file = OpenOptions::new().append(true).open(&path)?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    fn write(&self, message: &str) {
        if let Ok(mut file) = self.file.lock() {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default();
            let _ = writeln!(file, "[{}] {message}", timestamp.as_secs_f64());
            let _ = file.flush();
        }
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
    fn startup_log_flushes_entries_and_overwrites_previous_launch() {
        let directory = std::env::temp_dir().join(format!(
            "xmljson-viewer-diagnostics-test-{}",
            std::process::id()
        ));
        let first = StartupLog::create(&directory).unwrap();
        first.write("Starting viewer");
        first.write("ERROR: Could not start listener");
        let text = fs::read_to_string(&first.path).unwrap();
        assert!(text.contains("Starting viewer"));
        assert!(text.contains("ERROR: Could not start listener"));
        let second = StartupLog::create(&directory).unwrap();
        assert_eq!(first.path, second.path);
        assert_eq!(second.path.file_name().unwrap(), "startup.log");
        assert!(fs::read_to_string(&second.path).unwrap().is_empty());
        second.write("New launch");
        first.write("Existing instance still running");
        let text = fs::read_to_string(&second.path).unwrap();
        assert!(text.contains("New launch"));
        assert!(text.contains("Existing instance still running"));
        assert!(!text.contains("Could not start listener"));
        assert!(!text.contains('\0'));
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        drop(first);
        drop(second);
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
