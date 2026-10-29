# Salle

A GTK app for the VMs that [maitred](../maitre/README.md) runs. It lists them
with their state, starts and stops them, creates new ones from an existing
disk image, edits their settings, shows how to reach a running one, and shows
its logs. Written in Vala with GTK 4, libadwaita and Blueprint.

Salle is a client: maitred does the work, and Salle talks to it over D-Bus
(`org.garnish.Maitre`, started by D-Bus activation on the first call). VM
settings are the files in `~/.config/garnish/vms/`, which Salle reads and
writes itself, since maitred has no API for them. Building images is left to
mkosi.

## Building

Dependencies: `meson`, `valac`, `blueprint-compiler`, `gtk4 >= 4.18`,
`libadwaita-1 >= 1.8`, `libpulse` (with its GLib main loop). At runtime, maitred (see its README, Install).

```sh
meson setup build -Dprofile=development
ninja -C build
GSETTINGS_SCHEMA_DIR=build/data ./build/src/salle
meson test -C build   # the configuration file code, desktop file, metainfo and schema
```

On Arch, `makepkg -si` in `host/garnish` builds and installs it from this
directory, as the `salle` package, along with maitre and carte (`garnish-host`).

### Code style

Vala sources are formatted with [Uncrustify](https://github.com/uncrustify/uncrustify)
and checked with [vala-lint](https://github.com/vala-lang/vala-lint), like
Pipoca's.

```sh
build-aux/check-style.sh         # check
build-aux/check-style.sh --fix   # reformat in place
```

## What it does

- **List**: every VM with a configuration file, with its state (stopped,
  starting, booting, running, shutting down) and a start or shut down
  button. The state follows maitred's `VmStateChanged`, so VMs started or
  stopped elsewhere (`maitre start`) show up too, as do files added, changed or
  removed in the directory.
- **Start**: `StartVm`. If it fails, maitred's message (with the end of
  crosvm's log) is shown, with a way to the logs and, for a configuration
  error, the settings.
- **Shut Down**: `StopVm`, the ACPI power button. **Force Off**: `KillVm`,
  after asking.
- **Running VMs**: the ssh command (`ssh <user>@vsock/<cid>`, to copy or to
  open in a terminal through `xdg-terminal-exec`), the address, tap, CID and
  crosvm's PID.
- **Sound**: for a running VM with `Audio=yes`, a volume slider and a mute
  button. They set the volume of the VM's stream on the host, through
  PipeWire's PulseAudio server (`node.name=garnish.<name>`, which maitred
  sets). The stream only exists while the guest has its sound card open,
  which it closes a few seconds after going quiet. While it's gone, the
  slider shows the volume WirePlumber saved, and a change waits for the
  stream to come back (as long as Salle is open). The guest's own output
  should stay at full volume (`garnish-guest` starts it there), since the
  host can only turn it down.
- **New and Edit**: one dialog for the common settings (disk image, CPUs,
  memory, network, Wayland, app menu, sound, microphone, shared home, guest
  user) and, under Advanced, kernel, initrd, extra kernel parameters, GPU
  context types and extra crosvm arguments. The kernel and initrd default to
  the files next to the image, as in maitred.
- **Delete**: removes the configuration file; the image and logs stay.
- **Logs**: `console.log`, `crosvm.log`, `vhost-device-sound.log` and
  `carte.log` from `~/.local/state/garnish/<name>/`, followed as they grow,
  with terminal escape sequences removed.

## Configuration files

Salle edits the files in place: changing a setting rewrites only the lines of
that setting, so comments, settings it doesn't show (`KernelCommandLine=`,
`GID=`) and their order stay. A new VM's file gets the main settings written
out. Settings in drop-ins (`<name>.conf.d/*.conf`) are shown, but saving only
changes the main file, so a drop-in still wins. The parser follows maitre's
(`src/ini.rs`); maitred still checks everything when the VM starts.

## Not done

- building images
- renaming a VM

## Code

| File | Contents |
|---|---|
| `src/services/ini-file.vala` | the configuration file format, edited in place |
| `src/models/vm-config.vala` | a VM's settings: defaults, loading, saving the changes |
| `src/models/vm.vala` | a VM and its state |
| `src/services/maitre.vala` | the D-Bus client and the list of VMs |
| `src/vm-actions.vala` | start, stop, edit, delete and the rest, with their dialogs |
| `src/window.*` | the sidebar and the page of the selected VM |
| `src/pages/vm-page.*` | a VM's state, connection details, settings summary and logs |
| `src/dialogs/vm-dialog.*` | creating a VM and editing its settings |
| `src/dialogs/log-dialog.*` | the log viewer |
| `src/widgets/vm-row.*` | a VM in the sidebar |
| `tests/test-config.vala` | tests for the configuration file code |
