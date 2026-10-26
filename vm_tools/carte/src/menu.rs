//! The host's .desktop files for a VM's applications.
//!
//! Everything in a [`App`] comes from the guest, so it only ends up in
//! display keys (Name, Comment, ...), escaped. Exec= is built here from the
//! VM name and app ID, which are checked by [`proto::is_valid_vm`] and
//! [`proto::is_valid_id`].

use std::fmt::Write;
use std::path::Path;

use crate::proto::{self, App};

/// Prefix sommelier gives the app_id of a native Wayland window from `vm`
/// (NATIVE_WAYLAND_APPLICATION_ID_FORMAT in sommelier.h).
pub fn window_app_id(vm: &str, app_id: &str) -> String {
    format!("org.chromium.guest_os.{vm}.wayland.{app_id}")
}

/// File name in ~/.local/share/applications. VM names have no '-', so the
/// prefix carte-<vm>- belongs to exactly one VM.
pub fn file_name(vm: &str, id: &str) -> String {
    format!("{}{id}.desktop", file_prefix(vm))
}

pub fn file_prefix(vm: &str) -> String {
    format!("carte-{vm}-")
}

/// The .desktop file for `app`. `exe` is the carte binary, which Exec= runs
/// as `carte launch <vm> <id>`.
pub fn render(vm: &str, app: &App, icon: Option<&Path>, exe: &Path) -> String {
    debug_assert!(proto::is_valid_vm(vm) && proto::is_valid_id(&app.id));
    let mut s = String::from("[Desktop Entry]\nType=Application\n");
    for (locale, name) in &app.name {
        entry(&mut s, "Name", locale, &escape(&format!("{name} ({vm})")));
    }
    for (locale, name) in &app.generic_name {
        entry(&mut s, "GenericName", locale, &escape(name));
    }
    for (locale, comment) in &app.comment {
        entry(&mut s, "Comment", locale, &escape(comment));
    }
    // Searching for the VM's name finds all of its applications.
    let mut keywords = app.keywords.clone();
    keywords.entry(String::new()).or_default().push(vm.to_owned());
    for (locale, words) in &keywords {
        entry(&mut s, "Keywords", locale, &escape_list(words));
    }
    if !app.categories.is_empty() {
        entry(&mut s, "Categories", "", &escape_list(&app.categories));
    }
    if let Some(icon) = icon {
        entry(&mut s, "Icon", "", &escape(&icon.to_string_lossy()));
    }
    let exec = format!("{} launch {vm} {}", quote_exec_arg(&exe.to_string_lossy()), app.id);
    entry(&mut s, "Exec", "", &escape(&exec));
    // GNOME matches windows to applications by app_id, and Wayland apps set
    // theirs to the desktop file ID, or to StartupWMClass when that differs.
    let app_id = app.startup_wm_class.as_deref().unwrap_or(&app.id);
    entry(&mut s, "StartupWMClass", "", &escape(&window_app_id(vm, app_id)));
    entry(&mut s, "X-Carte-VM", "", vm);
    entry(&mut s, "X-Carte-Id", "", &app.id);
    s
}

fn entry(s: &mut String, key: &str, locale: &str, value: &str) {
    let locale_ok = locale.bytes().all(|b| b.is_ascii_alphanumeric() || b"_@.-".contains(&b));
    if !locale_ok {
        return;
    }
    s.push_str(key);
    if !locale.is_empty() {
        let _ = write!(s, "[{locale}]");
    }
    let _ = writeln!(s, "={value}");
}

/// Escapes a string value. Control characters other than \n \t \r are dropped.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for (i, c) in value.chars().enumerate() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            ' ' if i == 0 => out.push_str("\\s"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

fn escape_list(items: &[String]) -> String {
    let mut out = String::new();
    for item in items.iter().filter(|i| !i.is_empty()) {
        out.push_str(&escape(item).replace(';', "\\;"));
        out.push(';');
    }
    out
}

/// Quotes one Exec= argument if it needs it. The result still has to go
/// through [`escape`] like any other value.
fn quote_exec_arg(arg: &str) -> String {
    const RESERVED: &str = " \t\n\"'\\><~|&;$*?#()`";
    if !arg.is_empty() && !arg.chars().any(|c| RESERVED.contains(c)) {
        return arg.to_owned();
    }
    let mut out = String::from("\"");
    for c in arg.chars() {
        if "\"`$\\".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::DesktopEntry;

    fn app() -> App {
        let mut app = App { id: "org.gnome.Calculator".into(), ..Default::default() };
        app.name.insert("".into(), "Calculator".into());
        app.name.insert("de".into(), "Rechner".into());
        app.name.insert("x]\nExec=evil".into(), "nope".into());
        app.comment.insert("".into(), "line\nExec=evil".into());
        app.keywords.insert("".into(), vec!["a;b".into(), "c".into()]);
        app.categories = vec!["Utility".into()];
        app
    }

    #[test]
    fn renders() {
        let text = render("debian", &app(), Some(Path::new("/i/calc.svg")), Path::new("/usr/bin/carte"));
        assert_eq!(
            text,
            "[Desktop Entry]\nType=Application\n\
             Name=Calculator (debian)\nName[de]=Rechner (debian)\n\
             Comment=line\\nExec=evil\n\
             Keywords=a\\;b;c;debian;\n\
             Categories=Utility;\n\
             Icon=/i/calc.svg\n\
             Exec=/usr/bin/carte launch debian org.gnome.Calculator\n\
             StartupWMClass=org.chromium.guest_os.debian.wayland.org.gnome.Calculator\n\
             X-Carte-VM=debian\nX-Carte-Id=org.gnome.Calculator\n"
        );
    }

    #[test]
    fn exec_round_trip() {
        // What we write parses back to the same command with our own parser.
        let exe = Path::new("/home/me/my bin/$carte\\x");
        let text = render("debian", &app(), None, exe);
        let entry = DesktopEntry::parse(&text, "/x.desktop".into(), "x".into());
        assert_eq!(
            entry.exec_argv().unwrap(),
            [exe.to_str().unwrap(), "launch", "debian", "org.gnome.Calculator"]
        );
        assert_eq!(entry.comment[""], "line\nExec=evil");
        assert_eq!(entry.keywords[""], ["a;b", "c", "debian"]);
    }
}
