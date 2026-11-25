use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub const DEFAULT_TREE_WIDTH: f32 = 400.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub tree_width: f32,
    pub open_files_in_new_window: bool,
    pub search_case_sensitive: bool,
    pub theme: String,
    pub decode_on_open: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            tree_width: DEFAULT_TREE_WIDTH,
            open_files_in_new_window: false,
            search_case_sensitive: true,
            theme: "dark".into(),
            decode_on_open: false,
        }
    }
}

fn directory() -> PathBuf {
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    };
    base.unwrap_or_else(std::env::temp_dir)
        .join("xmljson-viewer")
}

fn path() -> PathBuf {
    directory().join("settings.json")
}

pub fn load() -> Settings {
    load_from(&path())
}

/// Re-reads the file before writing so windows only overwrite the fields they change.
pub fn update(change: impl FnOnce(&mut Settings)) {
    let path = path();
    let mut settings = load_from(&path);
    change(&mut settings);
    if let Err(error) = save_to(&path, &settings) {
        crate::diagnostics::record(&format!("Could not save settings: {error}"));
    }
}

fn load_from(path: &Path) -> Settings {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_to(path: &Path, settings: &Settings) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(settings)?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&temporary, json)?;
    fs::rename(&temporary, path).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_partial_files_fall_back_to_defaults() {
        let directory = std::env::temp_dir().join(format!(
            "xmljson-viewer-settings-test-{}",
            std::process::id()
        ));
        let path = directory.join("settings.json");
        assert_eq!(load_from(&path), Settings::default());
        fs::create_dir_all(&directory).unwrap();
        fs::write(&path, r#"{"tree_width": 512.0}"#).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded.tree_width, 512.0);
        assert!(!loaded.open_files_in_new_window);
        assert!(loaded.search_case_sensitive);
        let changed = Settings {
            open_files_in_new_window: true,
            ..loaded
        };
        save_to(&path, &changed).unwrap();
        assert_eq!(load_from(&path), changed);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_dir_all(directory).unwrap();
    }
}
