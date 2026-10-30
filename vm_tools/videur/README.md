# videur

A Wayland proxy between a crosvm guest and the host compositor that
controls which way copied data may go. maitred starts one per VM with
`[Machine] Clipboard=` set to anything but `both` (see the
[maitre README](../maitre/README.md)), and gives crosvm its socket instead
of the host's.

```sh
videur --display $XDG_RUNTIME_DIR/wayland-0 --listen /path/wayland.sock --clipboard none
```

| `--clipboard` | The host pastes what the guest copies | The guest pastes what the host copies |
|---|---|---|
| `both` | yes | yes |
| `to-host` | yes | no |
| `from-host` | no | yes |
| `none` | no | no |

Copy and paste between the guest's own apps works in every mode. Drag and
drop follows the same rules, but hasn't been tried end to end yet.

## How

sommelier passes the guest's clipboard to the host compositor, so even a
copy and paste inside the guest goes through it. videur reads every
message:

- An offer from the compositor is held back until it's known whether the
  guest owns that selection (or drag). If it does, the offer is passed on,
  and pasting from it asks the guest's own source directly: the compositor
  is left out. If not, the data is a host app's: without `from-host`, the
  guest gets an empty clipboard and the offer goes back to the compositor.
- Without `to-host`, the compositor's requests for the guest's data are
  dropped, and the host app pastes nothing. The host still sees that the
  guest copied something, and its types (`text/plain`...), and its own
  clipboard is replaced.

Taking a host offer for the guest's own can only make the guest paste its
own data: those pastes never reach the compositor.

Only what the guest is given can be used: requests for objects it wasn't
given, binds to globals it wasn't offered, or anything that doesn't parse
end the connection. This is enforced on the host, so it holds against a
guest whose root user talks to the GPU channel directly.

## Protocols

To know which file descriptors go with which message and which objects a
message creates, videur reads the protocol descriptions in
`/usr/share/wayland` and `/usr/share/wayland-protocols` (the `wayland` and
`wayland-protocols` packages), plus any `--protocols DIR`. Globals of other
protocols (e.g. GNOME's `gtk_shell1`) are hidden from the guest, and
versions are capped at the ones in those files. Everything sommelier uses
is there.

Always hidden: `ext_data_control_manager_v1` and
`zwlr_data_control_manager_v1`, which read the clipboard without focus, and
`gtk_primary_selection_device_manager`. The primary selection
(`zwp_primary_selection_device_manager_v1`) follows the same rules as the
clipboard; sommelier doesn't support it.

## Tests

`cargo test` covers the filter with the installed protocol files. End to
end, with sway headless, two GTK clients (one through videur) and
`wl-copy`/`wl-paste`, all four modes behave as in the table.
