//! Offline `.wbt` DEF pre-check. This is only a convenience for catching typos
//! before launching Webots; the adapter's online validation remains normative.

use std::collections::HashSet;
use std::path::Path;

/// Return the DEF names from `defs` that do not occur as `DEF <name>` in the
/// world file. Whitespace (including newlines) between `DEF` and the name is
/// tolerated. If the file cannot be read, every requested DEF is reported
/// missing.
pub fn check_world_defs(world_path: &Path, defs: &[&str]) -> Vec<String> {
    let text = match std::fs::read_to_string(world_path) {
        Ok(text) => text,
        Err(_) => return defs.iter().map(|s| (*s).to_string()).collect(),
    };
    let found = scan_defs(&text);
    let mut seen = HashSet::new();
    let mut missing = Vec::new();
    for name in defs {
        if !found.contains(*name) && seen.insert((*name).to_string()) {
            missing.push((*name).to_string());
        }
    }
    missing
}

/// Extract every `DEF <identifier>` token from Webots world text.
pub(crate) fn scan_defs(text: &str) -> HashSet<String> {
    fn is_word(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'_'
    }

    let bytes = text.as_bytes();
    let mut found = HashSet::new();
    let mut i = 0usize;
    while i + 3 <= bytes.len() {
        let is_def = &bytes[i..i + 3] == b"DEF"
            && (i == 0 || !is_word(bytes[i - 1]))
            && (i + 3 == bytes.len() || !is_word(bytes[i + 3]));
        if is_def {
            let mut j = i + 3;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            let start = j;
            while j < bytes.len() && is_word(bytes[j]) {
                j += 1;
            }
            if j > start {
                found.insert(String::from_utf8_lossy(&bytes[start..j]).into_owned());
                i = j;
                continue;
            }
        }
        i += 1;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_defs_with_mixed_whitespace() {
        let text = "DEF       BASE  Solid {\n}\n  DEF arm Link {\n}\nDEF_OTHER notadef";
        let found = scan_defs(text);
        assert!(found.contains("BASE"));
        assert!(found.contains("arm"));
        assert!(!found.contains("notadef"));
    }

    #[test]
    fn missing_list_is_stable() {
        let dir = std::env::temp_dir().join(format!("janus-b3-world-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tiny.wbt");
        std::fs::write(&path, "DEF A Solid {}\nDEF B Solid {}\n").unwrap();
        assert_eq!(check_world_defs(&path, &["A", "C", "B", "C"]), vec!["C"]);
        let _ = std::fs::remove_file(&path);
    }
}
