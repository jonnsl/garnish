//! Finding the file for an Icon= value. A simplified icon theme lookup, in
//! the order garcon uses: themes' apps/ directories at the requested size,
//! then other sizes, then pixmaps. No index.theme parsing.

use std::path::{Path, PathBuf};

const THEMES: &[&str] = &["hicolor", "Adwaita", "gnome", "breeze"];
const SIZES: &[u32] = &[1024, 512, 256, 192, 128, 96, 72, 64, 48, 32, 24, 16];

/// The icon file and its format ("png" or "svg").
pub fn find(icon: &str, size: u32, data_dirs: &[PathBuf]) -> Option<(PathBuf, &'static str)> {
    let path = Path::new(icon);
    if path.is_absolute() {
        return format_of(path)
            .filter(|_| path.is_file())
            .map(|f| (path.to_owned(), f));
    }
    if icon.contains('/') {
        return None;
    }
    let name = icon
        .strip_suffix(".png")
        .or_else(|| icon.strip_suffix(".svg"))
        .unwrap_or(icon);

    // The requested size, a scalable icon, then larger sizes from the
    // closest up, then smaller ones from the closest down.
    let mut candidates = vec![
        format!("{size}x{size}/apps/{name}.png"),
        format!("scalable/apps/{name}.svg"),
    ];
    let larger = SIZES.iter().rev().filter(|&&s| s > size);
    let smaller = SIZES.iter().filter(|&&s| s < size);
    candidates.extend(
        larger
            .chain(smaller)
            .map(|s| format!("{s}x{s}/apps/{name}.png")),
    );

    let themed = THEMES.iter().flat_map(|theme| {
        data_dirs
            .iter()
            .map(move |dir| dir.join("icons").join(theme))
            .filter(|d| d.is_dir())
    });
    for theme_dir in themed {
        if let Some(found) = candidates
            .iter()
            .map(|c| theme_dir.join(c))
            .find(|p| p.is_file())
        {
            let format = format_of(&found)?;
            return Some((found, format));
        }
    }
    for dir in data_dirs {
        for sub in ["icons", "pixmaps"] {
            for format in ["png", "svg"] {
                let path = dir.join(sub).join(format!("{name}.{format}"));
                if path.is_file() {
                    return Some((path, format));
                }
            }
        }
    }
    None
}

fn format_of(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()? {
        "png" => Some("png"),
        "svg" => Some("svg"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn lookup_order() {
        let root = std::env::temp_dir().join(format!("carte-icons-{}", std::process::id()));
        let share = root.join("share");
        let touch = |rel: &str| {
            let p = share.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, b"x").unwrap();
            p
        };
        let dirs = vec![share.clone()];
        let small = touch("icons/hicolor/48x48/apps/app.png");
        assert_eq!(find("app", 256, &dirs), Some((small, "png")));
        let large = touch("icons/hicolor/512x512/apps/app.png");
        assert_eq!(find("app", 256, &dirs), Some((large, "png")));
        let svg = touch("icons/hicolor/scalable/apps/app.svg");
        assert_eq!(find("app", 256, &dirs), Some((svg.clone(), "svg")));
        assert_eq!(find("app.svg", 256, &dirs), Some((svg.clone(), "svg")));
        assert_eq!(find(svg.to_str().unwrap(), 256, &dirs), Some((svg, "svg")));
        let pixmap = touch("pixmaps/other.png");
        assert_eq!(find("other", 256, &dirs), Some((pixmap, "png")));
        assert_eq!(find("missing", 256, &dirs), None);
        assert_eq!(find("../x", 256, &dirs), None);
        fs::remove_dir_all(root).unwrap();
    }
}
