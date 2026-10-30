//! What the host keeps about a VM's applications besides the menu: the last
//! list from the agent, with the icons fetched for it, and the applications
//! the user took out of the menu. Salle reads both to list a VM's
//! applications, also while it is off.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::proto::{self, App};
use crate::xdg;

/// `~/.local/share/carte/apps/<vm>.json`
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct Catalog {
    pub vm: String,
    /// Whether `sync` puts this VM's applications in the menu (no
    /// `--no-menu`), so `hide` and `show` know whether to write launchers.
    pub menu: bool,
    pub apps: Vec<CatalogApp>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct CatalogApp {
    #[serde(flatten)]
    pub app: App,
    /// The fetched icon, in [`icon_dir`].
    #[serde(default)]
    pub icon_file: Option<PathBuf>,
}

pub fn catalog_file(vm: &str) -> PathBuf {
    xdg::data_home().join("carte/apps").join(format!("{vm}.json"))
}

pub fn icon_dir(vm: &str) -> PathBuf {
    xdg::data_home().join("carte/icons").join(vm)
}

/// `~/.config/carte/<vm>.hidden`: the IDs of the applications left out of
/// the menu, one per line.
pub fn hidden_file(vm: &str) -> PathBuf {
    xdg::config_home().join("carte").join(format!("{vm}.hidden"))
}

/// Reads a hidden file. Blank lines, `#` comments and invalid IDs are skipped.
pub fn parse_hidden(text: &str) -> BTreeSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| proto::is_valid_id(l))
        .map(str::to_owned)
        .collect()
}

pub fn format_hidden(ids: &BTreeSet<String>) -> String {
    ids.iter().map(|id| format!("{id}\n")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_round_trip() {
        let ids = parse_hidden("org.gnome.Calculator\n\n# a comment\n  kde4-kate \n../x\n");
        assert_eq!(ids.iter().map(String::as_str).collect::<Vec<_>>(), ["kde4-kate", "org.gnome.Calculator"]);
        assert_eq!(format_hidden(&ids), "kde4-kate\norg.gnome.Calculator\n");
    }

    #[test]
    fn catalog_format() {
        let mut app = App { id: "org.gnome.Calculator".into(), ..Default::default() };
        app.name.insert("".into(), "Calculator".into());
        let catalog = Catalog {
            vm: "debian".into(),
            menu: true,
            apps: vec![CatalogApp { app, icon_file: Some("/i/calc.svg".into()) }],
        };
        let text = serde_json::to_string(&catalog).unwrap();
        // Salle reads these keys.
        assert!(text.starts_with(r#"{"vm":"debian","menu":true,"apps":[{"id":"org.gnome.Calculator","name":{"":"Calculator"},"#));
        assert!(text.ends_with(r#""icon_file":"/i/calc.svg"}]}"#));
        assert_eq!(serde_json::from_str::<Catalog>(&text).unwrap(), catalog);
    }
}
