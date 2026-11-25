use std::{
    fs::File,
    io::Read,
    path::{Component, Path},
};

use zip::{CompressionMethod, ZipArchive};

/// Caps keep a hostile or accidental archive from expanding without bound in memory.
pub const MAX_FILES: usize = 200;
const MAX_ENTRY_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;

pub struct TextEntry {
    pub name: String,
    pub text: String,
}

pub struct ArchiveRead {
    pub entries: Vec<TextEntry>,
    pub notice: String,
}

pub struct TextListing {
    pub names: Vec<String>,
    pub skipped: usize,
}

pub fn is_zip(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
}

/// Names of the text files, without the file-count or size caps used when opening.
/// File bodies are discarded, so a large archive is not kept in memory just to be counted.
pub fn list_text_files(path: &Path, cancelled: &impl Fn() -> bool) -> Result<TextListing, String> {
    let mut names = Vec::new();
    let skipped = scan(path, cancelled, |entry| {
        names.push(entry.name);
        true
    })?;
    Ok(TextListing { names, skipped })
}

/// Reads UTF-8 files from a zip in memory. Entries are never written to disk, so a
/// crafted path inside the archive cannot replace a local file.
pub fn read_text_files(path: &Path, cancelled: &impl Fn() -> bool) -> Result<ArchiveRead, String> {
    read_limited(path, None, cancelled)
}

/// Reads only the text files marked in `include`, in archive order. The flags follow
/// the same order as `list_text_files`.
pub fn read_selected(
    path: &Path,
    include: &[bool],
    cancelled: &impl Fn() -> bool,
) -> Result<ArchiveRead, String> {
    read_limited(path, Some(include), cancelled)
}

fn read_limited(
    path: &Path,
    include: Option<&[bool]>,
    cancelled: &impl Fn() -> bool,
) -> Result<ArchiveRead, String> {
    let mut entries = Vec::new();
    let mut total = 0u64;
    let mut index = 0usize;
    let mut truncated = false;
    let skipped = scan(path, cancelled, |entry| {
        let take = include
            .map(|flags| flags.get(index).copied().unwrap_or(false))
            .unwrap_or(true);
        index += 1;
        if !take {
            return true;
        }
        if entries.len() >= MAX_FILES {
            truncated = true;
            return false;
        }
        let next_total = total.saturating_add(entry.text.len() as u64);
        if next_total > MAX_TOTAL_BYTES {
            truncated = true;
            return false;
        }
        total = next_total;
        entries.push(entry);
        true
    })?;
    let skipped = if include.is_some() { 0 } else { skipped };
    Ok(ArchiveRead {
        notice: notice(entries.is_empty() && include.is_none(), skipped, truncated),
        entries,
    })
}

/// `visit` returns false to stop. The returned count is entries that were not text.
fn scan(
    path: &Path,
    cancelled: &impl Fn() -> bool,
    mut visit: impl FnMut(TextEntry) -> bool,
) -> Result<usize, String> {
    let file = File::open(path).map_err(|error| format!("Could not open archive: {error}"))?;
    let mut archive =
        ZipArchive::new(file).map_err(|error| format!("Could not read archive: {error}"))?;
    let mut skipped = 0usize;
    for index in 0..archive.len() {
        if cancelled() {
            break;
        }
        let entry = match archive.by_index(index) {
            Ok(entry) => entry,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if entry.is_dir() {
            continue;
        }
        if entry.is_symlink() || entry.encrypted() {
            skipped += 1;
            continue;
        }
        let Some(name) = entry_name(&entry) else {
            skipped += 1;
            continue;
        };
        if entry.size() > MAX_ENTRY_BYTES {
            skipped += 1;
            continue;
        }
        let mut bytes = Vec::new();
        if entry
            .take(MAX_ENTRY_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() as u64 > MAX_ENTRY_BYTES
            || bytes.contains(&0)
        {
            skipped += 1;
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            skipped += 1;
            continue;
        };
        if !visit(TextEntry { name, text }) {
            break;
        }
    }
    Ok(skipped)
}

fn entry_name(entry: &zip::read::ZipFile<'_, File>) -> Option<String> {
    let path = entry.enclosed_name()?;
    let name = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    (!name.is_empty()).then_some(name)
}

fn notice(empty: bool, skipped: usize, truncated: bool) -> String {
    let mut parts = Vec::new();
    if empty {
        parts.push("The archive has no text files".to_string());
    }
    if truncated {
        parts.push("Stopped before opening every text file".to_string());
    }
    if skipped > 0 {
        parts.push(format!(
            "Skipped {skipped} entries that could not be opened as text"
        ));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Write, time::SystemTime};
    use zip::write::SimpleFileOptions;

    fn temp_zip(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        path
    }

    fn write_archive(path: &Path, files: &[(&str, &[u8])], directory: Option<&str>) {
        let file = File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = || SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        if let Some(directory) = directory {
            zip.add_directory(directory, options()).unwrap();
        }
        for (name, bytes) in files {
            zip.start_file(*name, options()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn reads_nested_text_and_skips_directories_binary_and_unsafe_paths() {
        let path = temp_zip("xmljson-archive");
        write_archive(
            &path,
            &[
                ("nested/a.xml", b"<root/>"),
                ("b.json", br#"{"n":1}"#),
                ("bad.bin", &[0, 255, 254]),
                ("../secret.xml", b"<no/>"),
            ],
            Some("empty"),
        );

        let read = read_text_files(&path, &|| false).unwrap();
        let names: Vec<_> = read
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, ["nested/a.xml", "b.json"]);
        assert!(read.notice.contains("Skipped"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn stops_after_the_file_cap() {
        let path = temp_zip("xmljson-archive-cap");
        let files: Vec<(String, Vec<u8>)> = (0..=MAX_FILES)
            .map(|index| (format!("f{index}.json"), br#"{"ok":true}"#.to_vec()))
            .collect();
        let file = File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        for (name, bytes) in &files {
            let options =
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            zip.start_file(name.as_str(), options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();

        let read = read_text_files(&path, &|| false).unwrap();
        assert_eq!(read.entries.len(), MAX_FILES);
        assert!(
            read.notice
                .contains("Stopped before opening every text file")
        );
        let listed = list_text_files(&path, &|| false).unwrap();
        assert_eq!(listed.names.len(), MAX_FILES + 1);
        let mut include = vec![false; listed.names.len()];
        include[MAX_FILES] = true;
        let selected = read_selected(&path, &include, &|| false).unwrap();
        assert_eq!(selected.entries.len(), 1);
        assert_eq!(selected.entries[0].name, format!("f{MAX_FILES}.json"));
        assert!(selected.notice.is_empty());
        fs::remove_file(path).unwrap();
    }
}
