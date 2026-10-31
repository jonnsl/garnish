//! systemd/mkosi style configuration files.
//!
//! `[Section]` headers and `Key=Value` lines. Lines starting with `#` or `;`
//! are comments. A value continues on the next line if the line ends with a
//! backslash (systemd) or if the next line is indented (mkosi). Every
//! assignment is kept, in order, so the caller decides whether a later one
//! replaces or extends an earlier one.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Entry {
    pub section: String,
    pub key: String,
    pub value: String,
    /// The file the assignment is in, for relative paths and messages.
    pub file: PathBuf,
    pub line: usize,
}

impl Entry {
    /// An error about this assignment, with its location.
    pub fn error(&self, msg: impl fmt::Display) -> String {
        format!("{}:{}: {}={}: {msg}", self.file.display(), self.line, self.key, self.value)
    }

    /// The directory relative paths in this assignment are resolved against.
    pub fn dir(&self) -> &Path {
        self.file.parent().unwrap_or(Path::new("/"))
    }
}

pub fn parse(text: &str, file: &Path) -> Result<Vec<Entry>, String> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut section: Option<String> = None;
    // Whether an indented line continues the last entry (mkosi style).
    let mut continuable = false;
    // Whether the last line ended with a backslash (systemd style).
    let mut backslash = false;
    let err = |n: usize, msg: &str| format!("{}:{}: {msg}", file.display(), n);

    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let trimmed = raw.trim();
        let (content, ends_with_backslash) = match trimmed.strip_suffix('\\') {
            Some(rest) => (rest.trim_end(), true),
            None => (trimmed, false),
        };
        if backslash || (continuable && !trimmed.is_empty() && raw.starts_with(char::is_whitespace)) {
            if trimmed.starts_with(['#', ';']) {
                continue;
            }
            let last = entries.last_mut().unwrap();
            if !content.is_empty() {
                if !last.value.is_empty() {
                    last.value.push(' ');
                }
                last.value.push_str(content);
            }
            backslash = ends_with_backslash;
            continue;
        }
        if trimmed.is_empty() {
            continuable = false;
            continue;
        }
        if trimmed.starts_with(['#', ';']) {
            continue;
        }
        if let Some(name) = trimmed.strip_prefix('[') {
            let name = name.strip_suffix(']').ok_or_else(|| err(n, "missing ']'"))?;
            if name.is_empty() {
                return Err(err(n, "empty section name"));
            }
            section = Some(name.to_owned());
            continuable = false;
            continue;
        }
        let Some((key, value)) = content.split_once('=') else {
            return Err(err(n, &format!("expected Key=Value, got {trimmed:?}")));
        };
        let Some(section) = &section else {
            return Err(err(n, "assignment outside of a section"));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(err(n, "empty key"));
        }
        entries.push(Entry {
            section: section.clone(),
            key: key.to_owned(),
            value: value.trim().to_owned(),
            file: file.to_owned(),
            line: n,
        });
        continuable = true;
        backslash = ends_with_backslash;
    }
    Ok(entries)
}

/// Reads `path` and then the drop-ins in `<path>.d/*.conf`, in name order.
pub fn load(path: &Path) -> io::Result<Result<Vec<Entry>, String>> {
    let text = fs::read_to_string(path)?;
    let mut entries = match parse(&text, path) {
        Ok(entries) => entries,
        Err(e) => return Ok(Err(e)),
    };
    let mut dropin_dir = path.as_os_str().to_owned();
    dropin_dir.push(".d");
    let mut dropins: Vec<PathBuf> = match fs::read_dir(&dropin_dir) {
        Ok(dir) => dir
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|ext| ext == "conf"))
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    dropins.sort();
    for dropin in dropins {
        let text = fs::read_to_string(&dropin)?;
        match parse(&text, &dropin) {
            Ok(more) => entries.extend(more),
            Err(e) => return Ok(Err(e)),
        }
    }
    Ok(Ok(entries))
}

pub fn parse_bool(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "1" | "yes" | "y" | "true" | "t" | "on" => Some(true),
        "0" | "no" | "n" | "false" | "f" | "off" => Some(false),
        _ => None,
    }
}

/// Splits a value into words on whitespace. Single and double quotes group
/// words, and a backslash escapes the next character.
pub fn split_words(s: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (_, '\\') => {
                word.push(chars.next().ok_or("trailing backslash")?);
                in_word = true;
            }
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return Err("unterminated quote".into());
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

/// A size in bytes, with an optional K, M, G or T suffix (powers of 1024,
/// optionally followed by "iB" or "B").
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let digits = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, suffix) = s.split_at(digits);
    let num: u64 = num.parse().ok()?;
    let suffix = suffix.trim();
    let suffix = suffix.strip_suffix("iB").or_else(|| suffix.strip_suffix('B')).unwrap_or(suffix);
    let shift = match suffix.to_ascii_uppercase().as_str() {
        "" => 0,
        "K" => 10,
        "M" => 20,
        "G" => 30,
        "T" => 40,
        _ => return None,
    };
    num.checked_mul(1 << shift)
}

/// Expands a leading `~/` and makes `value` absolute relative to `dir`.
pub fn resolve_path(value: &str, dir: &Path, home: &Path) -> PathBuf {
    if value == "~" {
        home.to_owned()
    } else if let Some(rest) = value.strip_prefix("~/") {
        home.join(rest)
    } else {
        dir.join(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(text: &str) -> Vec<(String, String, String)> {
        parse(text, Path::new("/x/test.conf"))
            .unwrap()
            .into_iter()
            .map(|e| (e.section, e.key, e.value))
            .collect()
    }

    fn t(s: &str, k: &str, v: &str) -> (String, String, String) {
        (s.into(), k.into(), v.into())
    }

    #[test]
    fn sections_comments_and_repeats() {
        let text = "# comment\n[A]\nKey = value \n; other\nKey=\n\n[B]\nX=a=b\n";
        assert_eq!(kv(text), vec![t("A", "Key", "value"), t("A", "Key", ""), t("B", "X", "a=b")]);
    }

    #[test]
    fn continuations() {
        let text = "[A]\nList=one\n    two\n    # skipped\n    three\nNext=x \\\n  y\\\n z\n\n  After=blank\n";
        assert_eq!(
            kv(text),
            vec![t("A", "List", "one two three"), t("A", "Next", "x y z"), t("A", "After", "blank")]
        );
    }

    #[test]
    fn errors() {
        assert!(parse("Key=1\n", Path::new("t")).unwrap_err().contains("outside"));
        assert!(parse("[A\n", Path::new("t")).unwrap_err().contains("']'"));
        assert!(parse("[A]\nnope\n", Path::new("t")).unwrap_err().contains("Key=Value"));
    }

    #[test]
    fn words() {
        assert_eq!(split_words(r#" a  "b c" d\ e 'f"g' "" "#).unwrap(), ["a", "b c", "d e", "f\"g", ""]);
        assert!(split_words("\"open").is_err());
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("4G"), Some(4 << 30));
        assert_eq!(parse_size("512MiB"), Some(512 << 20));
        assert_eq!(parse_size("1024 M"), Some(1 << 30));
        assert_eq!(parse_size("100"), Some(100));
        assert_eq!(parse_size("4X"), None);
        assert_eq!(parse_size("G"), None);
    }

    #[test]
    fn paths() {
        let home = Path::new("/home/u");
        assert_eq!(resolve_path("~/a", Path::new("/c"), home), Path::new("/home/u/a"));
        assert_eq!(resolve_path("a/b", Path::new("/c"), home), Path::new("/c/a/b"));
        assert_eq!(resolve_path("/abs", Path::new("/c"), home), Path::new("/abs"));
    }
}
