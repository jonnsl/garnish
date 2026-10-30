//! carte: the host side of carte-agent.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::thread;
use std::time::Duration;

use carte::catalog::{self, Catalog, CatalogApp};
use carte::proto::{self, AppList, Reply, Request};
use carte::{menu, vsock, xdg};

const USAGE: &str = "\
usage: carte sync --cid <cid> [--no-menu]
                                  put the applications of the VM at <cid> in the
                                  application menu and keep them up to date;
                                  with --no-menu, only keep their list
       carte launch <vm> <app-id>  start an application in a running VM
       carte hide <vm> <app-id>    take an application out of the menu
       carte show <vm> <app-id>    put it back
       carte remove <vm>           remove a VM's applications from the menu";

const RETRY: Duration = Duration::from_secs(1);
const TIMEOUT: Duration = Duration::from_secs(10);
/// GNOME's app grid shows icons at up to 192 px (96 at 2x).
const ICON_SIZE: u32 = 256;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args[..] {
        ["sync", "--cid", cid] | ["sync", "--cid", cid, "--no-menu"] => match cid.parse() {
            Ok(cid) => sync(cid, args.len() == 3),
            Err(_) => Err(format!("invalid CID {cid}")),
        },
        ["launch", vm, id] => launch(vm, id),
        ["hide", vm, id] => set_hidden(vm, id, true),
        ["show", vm, id] => set_hidden(vm, id, false),
        ["remove", vm] => remove(vm),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("carte: {e}");
            ExitCode::FAILURE
        }
    }
}

fn check_vm(vm: &str) -> Result<(), String> {
    if proto::is_valid_vm(vm) {
        Ok(())
    } else {
        Err(format!("invalid VM name {vm:?}"))
    }
}

fn check_id(id: &str) -> Result<(), String> {
    if proto::is_valid_id(id) {
        Ok(())
    } else {
        Err(format!("invalid application ID {id:?}"))
    }
}

/// Where `sync` records the CID of a running VM, for `launch`.
fn cid_file(vm: &str) -> PathBuf {
    xdg::runtime_dir().join("carte").join(format!("{vm}.cid"))
}

fn applications_dir() -> PathBuf {
    xdg::data_home().join("applications")
}

fn sync(cid: u32, menu: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("finding the carte binary: {e}"))?;
    let mut waiting = false;
    loop {
        let conn = match vsock::connect(cid, proto::AGENT_PORT) {
            Ok(conn) => conn,
            Err(e) => {
                if !waiting {
                    eprintln!("carte: waiting for carte-agent on CID {cid} ({e})");
                    waiting = true;
                }
                thread::sleep(RETRY);
                continue;
            }
        };
        waiting = false;
        eprintln!("carte: connected to CID {cid}");
        let mut vm = None;
        let result = follow(&conn, cid, &exe, menu, &mut vm);
        if let Some(vm) = vm {
            remove_cid_file(&vm, cid);
        }
        match result {
            Ok(()) => eprintln!("carte: CID {cid} closed the connection"),
            Err(e) => eprintln!("carte: connection to CID {cid}: {e}"),
        }
        thread::sleep(RETRY);
    }
}

fn read_cid(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Removes the CID file of `vm` if it still points at `cid`. Another VM
/// with the same name may have replaced it, and that VM is still running.
fn remove_cid_file(vm: &str, cid: u32) {
    let path = cid_file(vm);
    if read_cid(&path) == Some(cid) {
        let _ = fs::remove_file(path);
    }
}

/// Applies application lists from the agent until the connection ends.
/// `vm` is set to the VM's name once it is known.
fn follow(conn: &File, cid: u32, exe: &Path, menu: bool, vm: &mut Option<String>) -> io::Result<()> {
    proto::send(&mut &*conn, &Request::Watch)?;
    let mut reader = BufReader::new(conn);
    while let Some(list) = proto::recv::<AppList>(&mut reader)? {
        if !proto::is_valid_vm(&list.vm) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("invalid VM name {:?}", list.vm)));
        }
        if vm.as_deref() != Some(list.vm.as_str()) {
            let path = cid_file(&list.vm);
            fs::create_dir_all(path.parent().unwrap())?;
            if let Some(other) = read_cid(&path).filter(|&c| c != cid) {
                eprintln!("carte: {}: replacing CID {other}, is another VM also named {}?", list.vm, list.vm);
            }
            fs::write(&path, format!("{cid}\n"))?;
            *vm = Some(list.vm.clone());
        }
        export(cid, &list, exe, menu);
    }
    Ok(())
}

/// Fetches the icons for `list`, records it in the catalog, and updates the
/// menu: launchers for the applications that aren't hidden, none at all
/// without `menu`.
fn export(cid: u32, list: &AppList, exe: &Path, menu: bool) {
    let vm = &list.vm;
    let icons = catalog::icon_dir(vm);
    if let Err(e) = fs::create_dir_all(&icons) {
        eprintln!("carte: creating {}: {e}", icons.display());
        return;
    }

    let mut fetcher = IconFetcher::new(cid);
    let mut icon_files = HashSet::new();
    let mut apps = Vec::new();
    for app in &list.apps {
        if !proto::is_valid_id(&app.id) || !app.name.contains_key("") {
            eprintln!("carte: {vm}: skipping invalid application {:?}", app.id);
            continue;
        }
        let icon = app.icon.as_ref().and_then(|_| update_icon(&mut fetcher, &icons, &app.id));
        if let Some(icon) = &icon {
            icon_files.insert(icon.file_name().unwrap().to_owned());
        }
        apps.push(CatalogApp { app: app.clone(), icon_file: icon });
    }
    remove_others(&icons, |_| true, &icon_files);

    let catalog = Catalog { vm: vm.clone(), menu, apps };
    if let Err(e) = write_catalog(&catalog) {
        eprintln!("carte: {vm}: writing the catalog: {e}");
    }
    // After the icons, so they're there when GNOME reads the .desktop files.
    let shown = update_menu(&catalog, exe);
    eprintln!("carte: {vm}: {} applications, {shown} in the menu", catalog.apps.len());
}

fn write_catalog(catalog: &Catalog) -> io::Result<()> {
    let path = catalog::catalog_file(&catalog.vm);
    fs::create_dir_all(path.parent().unwrap())?;
    write_if_changed(&path, &serde_json::to_vec(catalog)?)
}

fn read_hidden(vm: &str) -> std::collections::BTreeSet<String> {
    fs::read_to_string(catalog::hidden_file(vm)).map(|t| catalog::parse_hidden(&t)).unwrap_or_default()
}

/// Writes the launchers of the catalog's applications that aren't hidden and
/// removes the VM's others. Returns how many there are.
fn update_menu(catalog: &Catalog, exe: &Path) -> usize {
    let vm = &catalog.vm;
    let apps_dir = applications_dir();
    let mut desktop_files = HashSet::new();
    if catalog.menu {
        if let Err(e) = fs::create_dir_all(&apps_dir) {
            eprintln!("carte: creating {}: {e}", apps_dir.display());
            return 0;
        }
        let hidden = read_hidden(vm);
        for entry in catalog.apps.iter().filter(|a| !hidden.contains(&a.app.id)) {
            let name = menu::file_name(vm, &entry.app.id);
            let text = menu::render(vm, &entry.app, entry.icon_file.as_deref(), exe);
            if let Err(e) = write_if_changed(&apps_dir.join(&name), text.as_bytes()) {
                eprintln!("carte: writing {name}: {e}");
            }
            desktop_files.insert(name);
        }
    }
    remove_others(&apps_dir, |name| is_vms_desktop_file(&apps_dir, vm, name), &desktop_files);
    desktop_files.len()
}

/// Fetches the icon of `id` into `dir` and returns its path. Keeps the
/// existing file if the agent can't be reached.
fn update_icon(fetcher: &mut IconFetcher, dir: &Path, id: &str) -> Option<PathBuf> {
    let existing = || ["png", "svg"].map(|ext| dir.join(format!("{id}.{ext}"))).into_iter().find(|p| p.is_file());
    match fetcher.fetch(id) {
        Ok(Some((format, data))) => {
            let path = dir.join(format!("{id}.{format}"));
            if let Err(e) = write_if_changed(&path, &data) {
                eprintln!("carte: writing {}: {e}", path.display());
                return existing();
            }
            Some(path)
        }
        Ok(None) => None,
        Err(e) => {
            eprintln!("carte: icon for {id}: {e}");
            existing()
        }
    }
}

/// One connection for all the icons of an update.
struct IconFetcher {
    cid: u32,
    conn: Option<BufReader<File>>,
}

impl IconFetcher {
    fn new(cid: u32) -> Self {
        IconFetcher { cid, conn: None }
    }

    fn fetch(&mut self, id: &str) -> io::Result<Option<(String, Vec<u8>)>> {
        let result = self.try_fetch(id);
        if result.is_err() {
            // Don't reuse a connection that is in an unknown state.
            self.conn = None;
        }
        result
    }

    fn try_fetch(&mut self, id: &str) -> io::Result<Option<(String, Vec<u8>)>> {
        if self.conn.is_none() {
            let conn = vsock::connect(self.cid, proto::AGENT_PORT)?;
            vsock::set_timeout(&conn, TIMEOUT)?;
            self.conn = Some(BufReader::new(conn));
        }
        let reader = self.conn.as_mut().unwrap();
        proto::send(&mut reader.get_ref(), &Request::Icon { id: id.to_owned(), size: ICON_SIZE })?;
        let reply: Reply = proto::recv(reader)?.ok_or(io::ErrorKind::UnexpectedEof)?;
        if reply.error.is_some() {
            return Ok(None);
        }
        let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidData, format!("invalid icon {what}"));
        let format = reply.format.filter(|f| f == "png" || f == "svg").ok_or_else(|| invalid("format"))?;
        let len = reply.len.filter(|&l| l <= proto::MAX_ICON).ok_or_else(|| invalid("size"))?;
        let mut data = vec![0; len as usize];
        reader.read_exact(&mut data)?;
        Ok(Some((format, data)))
    }
}

/// Replaces `path` atomically, and only if the content differs, so GNOME
/// doesn't reload unchanged entries.
fn write_if_changed(path: &Path, data: &[u8]) -> io::Result<()> {
    if fs::read(path).is_ok_and(|old| old == data) {
        return Ok(());
    }
    // A dot file without .desktop at the end, so GNOME ignores it.
    let tmp = path.with_file_name(format!(".{}.tmp", path.file_name().unwrap().to_string_lossy()));
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)
}

/// Whether `name` in `dir` is a .desktop file carte wrote for `vm`.
fn is_vms_desktop_file(dir: &Path, vm: &str, name: &str) -> bool {
    name.starts_with("carte-")
        && fs::read_to_string(dir.join(name)).is_ok_and(|text| menu::belongs_to(vm, name, &text))
}

/// Removes the files in `dir` that match `ours` but aren't in `keep`.
fn remove_others(dir: &Path, ours: impl Fn(&str) -> bool, keep: &HashSet<impl AsRef<std::ffi::OsStr> + Eq + std::hash::Hash>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let keep: HashSet<&std::ffi::OsStr> = keep.iter().map(AsRef::as_ref).collect();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_str().is_some_and(&ours) && !keep.contains(name.as_os_str()) {
            if let Err(e) = fs::remove_file(entry.path()) {
                eprintln!("carte: removing {}: {e}", entry.path().display());
            }
        }
    }
}

fn launch(vm: &str, id: &str) -> Result<(), String> {
    let result = try_launch(vm, id);
    // Started from the application menu, nobody sees stderr.
    if let Err(e) = &result {
        let _ = Command::new("notify-send")
            .args(["--app-name=carte", &format!("Could not start {id}"), e])
            .status();
    }
    result
}

fn try_launch(vm: &str, id: &str) -> Result<(), String> {
    check_vm(vm)?;
    check_id(id)?;
    let cid = read_cid(&cid_file(vm)).ok_or_else(|| format!("{vm} is not running"))?;
    let conn = vsock::connect(cid, proto::AGENT_PORT).map_err(|e| format!("connecting to {vm} (CID {cid}): {e}"))?;
    vsock::set_timeout(&conn, TIMEOUT).map_err(|e| e.to_string())?;
    proto::send(&mut &conn, &Request::Launch { vm: vm.to_owned(), id: id.to_owned() })
        .map_err(|e| format!("talking to {vm}: {e}"))?;
    let reply: Reply = proto::recv(&mut BufReader::new(&conn))
        .map_err(|e| format!("talking to {vm}: {e}"))?
        .ok_or_else(|| format!("{vm} closed the connection"))?;
    match reply.error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Adds `id` to the VM's hidden applications or takes it out, and updates
/// the menu from the catalog, so it applies also while the VM is off.
fn set_hidden(vm: &str, id: &str, hide: bool) -> Result<(), String> {
    check_vm(vm)?;
    check_id(id)?;
    let path = catalog::hidden_file(vm);
    let mut hidden = read_hidden(vm);
    let changed = if hide { hidden.insert(id.to_owned()) } else { hidden.remove(id) };
    if changed {
        fs::create_dir_all(path.parent().unwrap())
            .and_then(|()| write_if_changed(&path, catalog::format_hidden(&hidden).as_bytes()))
            .map_err(|e| format!("writing {}: {e}", path.display()))?;
    }
    match fs::read(catalog::catalog_file(vm)) {
        Ok(data) => {
            let catalog: Catalog = serde_json::from_slice(&data).map_err(|e| format!("reading the catalog: {e}"))?;
            if catalog.vm != vm {
                return Err(format!("the catalog of {vm} is for {}", catalog.vm));
            }
            let exe = std::env::current_exe().map_err(|e| format!("finding the carte binary: {e}"))?;
            update_menu(&catalog, &exe);
        }
        // Never synced: the next sync reads the hidden file.
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("reading the catalog: {e}")),
    }
    Ok(())
}

fn remove(vm: &str) -> Result<(), String> {
    check_vm(vm)?;
    let apps_dir = applications_dir();
    let none: HashSet<String> = HashSet::new();
    remove_others(&apps_dir, |name| is_vms_desktop_file(&apps_dir, vm, name), &none);
    match fs::remove_file(catalog::catalog_file(vm)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(format!("removing the catalog: {e}")),
        _ => {}
    }
    match fs::remove_dir_all(catalog::icon_dir(vm)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(format!("removing icons: {e}")),
        _ => {}
    }
    Ok(())
}
