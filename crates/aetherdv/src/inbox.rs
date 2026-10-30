//! Where received files and forms land on disk.
//!
//! A peer chooses the file name, so it is treated as untrusted: reduced to a
//! portable base name (no separators, control characters, Windows-reserved
//! names or leading dots), never overwrites an existing file, and is written
//! to a temporary name first so a crash leaves no half-written file under
//! the final name.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Make `name` safe to use as a file name on Linux, macOS and Windows.
pub fn safe_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    s = s
        .trim_matches(|c: char| c == '.' || c.is_whitespace())
        .to_string();
    if s.is_empty() {
        s = "file".into();
    }
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        s.insert(0, '_');
    }
    // Keep names to 120 bytes, preserving a short extension.
    if s.len() > 120 {
        let ext = s
            .rfind('.')
            .filter(|&i| s.len() - i <= 10)
            .map(|i| s[i..].to_string())
            .unwrap_or_default();
        let mut cut = 120 - ext.len();
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s = format!("{}{ext}", &s[..cut]);
    }
    s
}

/// Write `data` into `dir` under a safe, unused variant of `name`
/// (`report.txt`, `report (1).txt`, ...). Returns the final path.
pub fn save_unique(dir: &Path, name: &str, data: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let name = safe_name(name);
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    let tmp = dir.join(format!(".aether-incoming-{}", crate::new_token()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    for n in 0..1000 {
        let candidate = if n == 0 {
            dir.join(&name)
        } else {
            dir.join(format!("{stem} ({n}){ext}"))
        };
        if candidate.exists() {
            continue;
        }
        std::fs::rename(&tmp, &candidate)?;
        return Ok(candidate);
    }
    let _ = std::fs::remove_file(&tmp);
    Err(std::io::Error::other("too many files with this name"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_names_are_neutralised() {
        assert_eq!(safe_name("report.txt"), "report.txt");
        assert_eq!(safe_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(safe_name("..\\boot.ini"), "_boot.ini");
        assert_eq!(safe_name(".bashrc"), "bashrc");
        assert_eq!(safe_name("a\u{0}b\nc"), "a_b_c");
        assert_eq!(safe_name("CON.txt"), "_CON.txt");
        assert_eq!(safe_name("com3"), "_com3");
        assert_eq!(safe_name("console.txt"), "console.txt");
        assert_eq!(safe_name("  ...  "), "file");
        let long = format!("{}.pdf", "é".repeat(100));
        let s = safe_name(&long);
        assert!(s.len() <= 120 && s.ends_with(".pdf"));
    }

    #[test]
    fn never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let a = save_unique(d.path(), "r.txt", b"one").unwrap();
        let b = save_unique(d.path(), "r.txt", b"two").unwrap();
        assert_eq!(a.file_name().unwrap(), "r.txt");
        assert_eq!(b.file_name().unwrap(), "r (1).txt");
        assert_eq!(std::fs::read(&a).unwrap(), b"one");
        assert_eq!(std::fs::read(&b).unwrap(), b"two");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 2);
    }
}
