//! Reading .desktop files (Desktop Entry Specification 1.5), as far as the
//! agent needs it. Which entries are shown follows garcon.

use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::proto::{self, Localized};

/// Larger files aren't desktop entries.
const MAX_SIZE: u64 = 1 << 20;
/// For OnlyShowIn= and NotShowIn=: the host desktop shows the entries.
const DESKTOP: &str = "GNOME";
/// Applications directories can have subdirectories (kde4/, ...).
pub const MAX_DEPTH: usize = 8;

#[derive(Debug, Default)]
pub struct DesktopEntry {
    pub path: PathBuf,
    pub id: String,
    pub kind: String,
    pub name: Localized,
    pub generic_name: Localized,
    pub comment: Localized,
    pub keywords: BTreeMap<String, Vec<String>>,
    pub categories: Vec<String>,
    pub icon: Option<String>,
    pub exec: Option<String>,
    pub try_exec: Option<String>,
    pub working_dir: Option<String>,
    pub terminal: bool,
    pub no_display: bool,
    pub hidden: bool,
    pub only_show_in: Vec<String>,
    pub not_show_in: Vec<String>,
    pub startup_wm_class: Option<String>,
}

impl DesktopEntry {
    pub fn load(path: &Path, id: String) -> io::Result<Self> {
        let mut text = String::new();
        File::open(path)?.take(MAX_SIZE).read_to_string(&mut text)?;
        Ok(Self::parse(&text, path.to_owned(), id))
    }

    pub fn parse(text: &str, path: PathBuf, id: String) -> Self {
        let mut e = DesktopEntry {
            path,
            id,
            ..Default::default()
        };
        let mut in_entry = false;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') {
                in_entry = line == "[Desktop Entry]";
                continue;
            }
            if !in_entry {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim_end(), value.trim_start());
            let (key, locale) = match key.split_once('[') {
                Some((key, rest)) => match rest.strip_suffix(']') {
                    Some(locale) => (key, locale),
                    None => continue,
                },
                None => (key, ""),
            };
            let localized = |map: &mut Localized| {
                map.insert(locale.to_owned(), unescape(value));
            };
            match (key, locale) {
                ("Name", _) => localized(&mut e.name),
                ("GenericName", _) => localized(&mut e.generic_name),
                ("Comment", _) => localized(&mut e.comment),
                ("Keywords", _) => {
                    e.keywords.insert(locale.to_owned(), split_list(value));
                }
                (_, "") => match key {
                    "Type" => e.kind = unescape(value),
                    "Categories" => e.categories = split_list(value),
                    "Icon" => e.icon = Some(unescape(value)),
                    "Exec" => e.exec = Some(unescape(value)),
                    "TryExec" => e.try_exec = Some(unescape(value)),
                    "Path" => e.working_dir = Some(unescape(value)),
                    "Terminal" => e.terminal = value == "true",
                    "NoDisplay" => e.no_display = value == "true",
                    "Hidden" => e.hidden = value == "true",
                    "OnlyShowIn" => e.only_show_in = split_list(value),
                    "NotShowIn" => e.not_show_in = split_list(value),
                    "StartupWMClass" => e.startup_wm_class = Some(unescape(value)),
                    _ => {}
                },
                _ => {}
            }
        }
        e.icon = e.icon.filter(|i| !i.is_empty());
        e.startup_wm_class = e.startup_wm_class.filter(|c| !c.is_empty());
        e
    }

    /// Whether the host should show this entry. garcon's rules, except that
    /// NoDisplay entries are always left out (we don't pass MIME types on)
    /// and so are Terminal=true ones (there's no terminal to run them in).
    pub fn should_export(&self) -> bool {
        let shown_here = |list: &[String]| list.iter().any(|d| d == DESKTOP);
        self.kind == "Application"
            && proto::is_valid_id(&self.id)
            && self.name.contains_key("")
            && self.exec.is_some()
            && !self.hidden
            && !self.no_display
            && !self.terminal
            && !self.categories.iter().any(|c| c == "Settings")
            && (self.only_show_in.is_empty() || shown_here(&self.only_show_in))
            && !shown_here(&self.not_show_in)
            && self
                .try_exec
                .as_deref()
                .is_none_or(|t| find_executable(t).is_some())
    }

    pub fn to_app(&self) -> proto::App {
        proto::App {
            id: self.id.clone(),
            name: self.name.clone(),
            generic_name: self.generic_name.clone(),
            comment: self.comment.clone(),
            keywords: self.keywords.clone(),
            categories: self.categories.clone(),
            icon: self.icon.clone(),
            startup_wm_class: self.startup_wm_class.clone(),
        }
    }

    /// The command line from Exec=, with field codes expanded for a launch
    /// without files or URLs.
    pub fn exec_argv(&self) -> Result<Vec<String>, String> {
        let exec = self.exec.as_deref().ok_or("no Exec= line")?;
        let mut args = Vec::new();
        let mut arg = String::new();
        // Distinguishes an empty quoted argument from no argument.
        let mut in_arg = false;
        let mut chars = exec.chars();
        while let Some(c) = chars.next() {
            match c {
                ' ' | '\t' | '\n' => {
                    if in_arg {
                        args.push(std::mem::take(&mut arg));
                        in_arg = false;
                    }
                }
                '"' => {
                    in_arg = true;
                    loop {
                        match chars.next() {
                            None => return Err(format!("unterminated quote in Exec={exec}")),
                            Some('"') => break,
                            Some('\\') => arg.push(chars.next().ok_or("Exec= ends with \\")?),
                            Some(c) => arg.push(c),
                        }
                    }
                }
                '%' => match chars.next() {
                    Some('%') => {
                        arg.push('%');
                        in_arg = true;
                    }
                    // Files and URLs: we never pass any.
                    Some('f' | 'F' | 'u' | 'U') => {}
                    // Deprecated, removed.
                    Some('d' | 'D' | 'n' | 'N' | 'v' | 'm') => {}
                    Some('i') => {
                        if let Some(icon) = &self.icon {
                            args.extend(["--icon".to_owned(), icon.clone()]);
                        }
                    }
                    Some('c') => {
                        arg.push_str(self.name.get("").map_or("", String::as_str));
                        in_arg = true;
                    }
                    Some('k') => {
                        arg.push_str(&self.path.to_string_lossy());
                        in_arg = true;
                    }
                    Some(c) => return Err(format!("unknown field code %{c} in Exec={exec}")),
                    None => return Err(format!("Exec= ends with %: {exec}")),
                },
                c => {
                    arg.push(c);
                    in_arg = true;
                }
            }
        }
        if in_arg {
            args.push(arg);
        }
        if args.is_empty() {
            return Err("empty Exec= line".into());
        }
        Ok(args)
    }
}

/// Undoes the escapes of string values: \s \n \t \r \\.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(c) => out.extend(['\\', c]),
            None => out.push('\\'),
        }
    }
    out
}

/// Splits a list value on ';', which can be escaped as "\;".
fn split_list(value: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut item = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(';') => item.push(';'),
                Some(c) => item.extend(['\\', c]),
                None => item.push('\\'),
            },
            ';' => items.push(unescape(&std::mem::take(&mut item))),
            c => item.push(c),
        }
    }
    items.push(unescape(&item));
    items.retain(|i| !i.is_empty());
    items
}

/// Resolves a command like the shell would: absolute paths as they are,
/// names through $PATH.
pub fn find_executable(name: &str) -> Option<PathBuf> {
    let is_executable = |p: &Path| {
        fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if name.contains('/') {
        let path = Path::new(name);
        return (path.is_absolute() && is_executable(path)).then(|| path.to_owned());
    }
    env::split_paths(&env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| is_executable(p))
}

/// The subdirectories of `dir`, recursively, `dir` included.
pub fn subdirectories(dir: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    walk(dir, 0, &mut dirs, &mut Vec::new());
    dirs
}

fn walk(dir: &Path, depth: usize, dirs: &mut Vec<PathBuf>, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    dirs.push(dir.to_owned());
    for entry in entries.flatten() {
        let path = entry.path();
        // fs::metadata follows symlinks: flatpak's exports are symlinks.
        match fs::metadata(&path) {
            Ok(m) if m.is_dir() && depth < MAX_DEPTH => walk(&path, depth + 1, dirs, files),
            Ok(m) if m.is_file() && path.extension().is_some_and(|e| e == "desktop") => {
                files.push(path)
            }
            _ => {}
        }
    }
}

/// The desktop file ID of `path` below `apps_dir`: kde4/kate.desktop is
/// kde4-kate.
fn desktop_id(apps_dir: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(apps_dir).ok()?.to_str()?;
    Some(relative.strip_suffix(".desktop")?.replace('/', "-"))
}

/// Every entry in the applications directories below `data_dirs`, in order
/// of importance. The first file with a given ID wins, even if it is hidden,
/// so a copy in ~/.local/share/applications can hide a system entry.
pub fn scan(data_dirs: &[PathBuf]) -> Vec<DesktopEntry> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for data_dir in data_dirs {
        let apps_dir = data_dir.join("applications");
        let mut files = Vec::new();
        walk(&apps_dir, 0, &mut Vec::new(), &mut files);
        files.sort();
        for file in files {
            let Some(id) = desktop_id(&apps_dir, &file) else {
                continue;
            };
            if !seen.insert(id.clone()) {
                continue;
            }
            match DesktopEntry::load(&file, id) {
                Ok(entry) => entries.push(entry),
                Err(e) => eprintln!("carte-agent: {}: {e}", file.display()),
            }
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> DesktopEntry {
        DesktopEntry::parse(
            text,
            "/usr/share/applications/app.desktop".into(),
            "app".into(),
        )
    }

    const CALCULATOR: &str = "\
# comment
[Desktop Entry]
Name=Calculator
Name[de]=Rechner
Comment = Perform calculations
Keywords=calculation;arithmetic;
Keywords[de]=Rechnen;
Categories=GNOME;GTK;Utility;
Exec=gnome-calculator %U
Icon=org.gnome.Calculator
Type=Application
StartupNotify=true

[Desktop Action shortcuts]
Name=Shortcuts
Exec=gnome-calculator --show-shortcuts
";

    #[test]
    fn parses_entry() {
        let e = parse(CALCULATOR);
        assert_eq!(e.name[""], "Calculator");
        assert_eq!(e.name["de"], "Rechner");
        assert_eq!(e.comment[""], "Perform calculations");
        assert_eq!(e.keywords[""], ["calculation", "arithmetic"]);
        assert_eq!(e.keywords["de"], ["Rechnen"]);
        assert_eq!(e.categories, ["GNOME", "GTK", "Utility"]);
        assert_eq!(e.icon.as_deref(), Some("org.gnome.Calculator"));
        // Not overwritten by the action group.
        assert_eq!(e.exec.as_deref(), Some("gnome-calculator %U"));
        assert!(e.should_export());
        assert_eq!(e.exec_argv().unwrap(), ["gnome-calculator"]);
    }

    #[test]
    fn filters() {
        let base = "[Desktop Entry]\nType=Application\nName=X\nExec=x\n";
        assert!(parse(base).should_export());
        for extra in [
            "NoDisplay=true",
            "Hidden=true",
            "Terminal=true",
            "Categories=Settings;",
            "OnlyShowIn=KDE;",
            "NotShowIn=GNOME;",
            "TryExec=/nonexistent/carte-test",
        ] {
            assert!(
                !parse(&format!("{base}{extra}\n")).should_export(),
                "{extra}"
            );
        }
        assert!(parse(&format!("{base}OnlyShowIn=KDE;GNOME;\n")).should_export());
        assert!(!parse("[Desktop Entry]\nType=Link\nName=X\nExec=x\n").should_export());
        assert!(!parse("[Desktop Entry]\nType=Application\nName[de]=X\nExec=x\n").should_export());
    }

    #[test]
    fn escapes() {
        let e = parse("[Desktop Entry]\nName=\\sa\\\\b\nKeywords=a\\;b;c;\n");
        assert_eq!(e.name[""], " a\\b");
        assert_eq!(e.keywords[""], ["a;b", "c"]);
    }

    #[test]
    fn exec_quoting() {
        let e = parse(concat!(
            "[Desktop Entry]\nName=My App\nIcon=my-icon\n",
            r#"Exec="/opt/my app/run" --name %c "a \\"b\\" \\$c" "" 100%% %i %F"#,
            "\n"
        ));
        assert_eq!(
            e.exec_argv().unwrap(),
            [
                "/opt/my app/run",
                "--name",
                "My App",
                r#"a "b" $c"#,
                "",
                "100%",
                "--icon",
                "my-icon"
            ]
        );
        assert!(parse("[Desktop Entry]\nExec=\"x\n").exec_argv().is_err());
        assert!(parse("[Desktop Entry]\nExec=x %z\n").exec_argv().is_err());
        assert!(parse("[Desktop Entry]\nExec=%f\n").exec_argv().is_err());
    }

    #[test]
    fn ids() {
        let dir = Path::new("/usr/share/applications");
        assert_eq!(
            desktop_id(dir, &dir.join("kde4/kate.desktop")).as_deref(),
            Some("kde4-kate")
        );
        assert_eq!(
            desktop_id(dir, &dir.join("a.b.desktop")).as_deref(),
            Some("a.b")
        );
    }
}
