use std::{
    fs::File,
    io::Read,
    path::{Component, Path},
};

use zip::{ZipArchive, read::ZipFile};

/// Caps keep a hostile or accidental archive from expanding without bound in memory.
pub const MAX_FILES: usize = 200;
const MAX_ENTRY_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
/// Listing only decompresses this much of each entry to tell text from binary.
const SNIFF_BYTES: u64 = 8 * 1024;

pub struct ListedEntry {
    /// Position in the archive, used to read exactly this entry later.
    pub index: usize,
    pub name: String,
}

pub struct TextListing {
    pub entries: Vec<ListedEntry>,
    pub skipped: usize,
}

pub struct TextEntry {
    pub name: String,
    pub text: String,
}

pub struct ArchiveRead {
    pub entries: Vec<TextEntry>,
    pub skipped: usize,
    pub truncated: bool,
}

pub fn is_zip(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
}

/// Entries that look like text from their first bytes, so a large archive is not
/// decompressed in full just to be counted.
pub fn list_text_files(path: &Path, cancelled: &impl Fn() -> bool) -> Result<TextListing, String> {
    let mut archive = open(path)?;
    let mut entries = Vec::new();
    let mut skipped = 0;
    for index in 0..archive.len() {
        if cancelled() {
            break;
        }
        let Ok(entry) = archive.by_index(index) else {
            skipped += 1;
            continue;
        };
        if entry.is_dir() {
            continue;
        }
        let Some(name) = usable_name(&entry) else {
            skipped += 1;
            continue;
        };
        let mut prefix = Vec::new();
        if entry.take(SNIFF_BYTES).read_to_end(&mut prefix).is_err() || !looks_like_text(&prefix) {
            skipped += 1;
            continue;
        }
        entries.push(ListedEntry { index, name });
    }
    Ok(TextListing { entries, skipped })
}

/// Reads the listed entries in memory, in the given order, up to the caps. Entries are
/// never written to disk, so a crafted path inside the archive cannot replace a local file.
pub fn read_entries(
    path: &Path,
    indices: &[usize],
    cancelled: &impl Fn() -> bool,
) -> Result<ArchiveRead, String> {
    let mut archive = open(path)?;
    let mut entries = Vec::new();
    let mut total = 0u64;
    let mut skipped = 0;
    let mut truncated = false;
    for &index in indices {
        if cancelled() {
            break;
        }
        if entries.len() >= MAX_FILES {
            truncated = true;
            break;
        }
        let Ok(entry) = archive.by_index(index) else {
            skipped += 1;
            continue;
        };
        let Some(name) = usable_name(&entry) else {
            skipped += 1;
            continue;
        };
        let mut bytes = Vec::new();
        // `take` bounds the read even if the entry lies about its size.
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
        let next_total = total + text.len() as u64;
        if next_total > MAX_TOTAL_BYTES {
            truncated = true;
            break;
        }
        total = next_total;
        entries.push(TextEntry { name, text });
    }
    Ok(ArchiveRead {
        entries,
        skipped,
        truncated,
    })
}

fn open(path: &Path) -> Result<ZipArchive<File>, String> {
    let file = File::open(path).map_err(|error| format!("Could not open archive: {error}"))?;
    ZipArchive::new(file).map_err(|error| format!("Could not read archive: {error}"))
}

/// Name for a regular, unencrypted entry within the size cap whose path stays inside the archive.
fn usable_name(entry: &ZipFile<'_, File>) -> Option<String> {
    if entry.is_symlink() || entry.encrypted() || entry.size() > MAX_ENTRY_BYTES {
        return None;
    }
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

fn looks_like_text(prefix: &[u8]) -> bool {
    if prefix.contains(&0) {
        return false;
    }
    match std::str::from_utf8(prefix) {
        Ok(_) => true,
        // Only a character cut at the end of the sniffed prefix.
        Err(error) => error.error_len().is_none(),
    }
}

pub fn notice(skipped: usize, truncated: bool) -> String {
    let mut parts = Vec::new();
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
    use zip::{CompressionMethod, write::SimpleFileOptions};

    fn temp_zip(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
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

    fn all(listing: &TextListing) -> Vec<usize> {
        listing.entries.iter().map(|entry| entry.index).collect()
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

        let listed = list_text_files(&path, &|| false).unwrap();
        assert_eq!(listed.skipped, 2);
        let read = read_entries(&path, &all(&listed), &|| false).unwrap();
        let names: Vec<_> = read
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, ["nested/a.xml", "b.json"]);
        assert_eq!(read.skipped, 0);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn listing_sniffs_a_prefix_and_reading_still_rejects_late_binary_data() {
        let path = temp_zip("xmljson-archive-sniff");
        let mut late_binary = vec![b'a'; SNIFF_BYTES as usize + 10];
        late_binary.push(0);
        // A multi-byte character straddling the sniffed prefix is still text.
        let mut split_char = vec![b'a'; SNIFF_BYTES as usize - 1];
        split_char.extend("é".as_bytes());
        write_archive(
            &path,
            &[("late.json", &late_binary), ("split.json", &split_char)],
            None,
        );

        let listed = list_text_files(&path, &|| false).unwrap();
        assert_eq!(listed.entries.len(), 2);
        let read = read_entries(&path, &all(&listed), &|| false).unwrap();
        assert_eq!(read.entries.len(), 1);
        assert_eq!(read.entries[0].name, "split.json");
        assert_eq!(read.skipped, 1);
        assert_eq!(
            notice(read.skipped, read.truncated),
            "Skipped 1 entries that could not be opened as text"
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn stops_after_the_file_cap_and_reads_selected_indices() {
        let path = temp_zip("xmljson-archive-cap");
        let files: Vec<(String, Vec<u8>)> = (0..=MAX_FILES)
            .map(|index| (format!("f{index}.json"), br#"{"ok":true}"#.to_vec()))
            .collect();
        let borrowed: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
            .collect();
        write_archive(&path, &borrowed, None);

        let listed = list_text_files(&path, &|| false).unwrap();
        assert_eq!(listed.entries.len(), MAX_FILES + 1);
        let read = read_entries(&path, &all(&listed), &|| false).unwrap();
        assert_eq!(read.entries.len(), MAX_FILES);
        assert!(read.truncated);
        let last = listed.entries[MAX_FILES].index;
        let selected = read_entries(&path, &[last], &|| false).unwrap();
        assert_eq!(selected.entries.len(), 1);
        assert_eq!(selected.entries[0].name, format!("f{MAX_FILES}.json"));
        assert!(!selected.truncated);
        fs::remove_file(path).unwrap();
    }
}
