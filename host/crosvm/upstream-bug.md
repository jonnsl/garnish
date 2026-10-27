# GPU device killed by seccomp when the host pastes into a guest (x86_64)

To file in crosvm's public component
(<https://issuetracker.google.com/issues?q=status:open%20componentid:1161302>)
and/or at <https://github.com/magma-gpu/rutabaga_gfx/issues>.

## Summary

On x86_64, the virtio-gpu device process is killed with SIGSYS the first time
a guest app pastes clipboard data owned by a host app through the
cross-domain context (sommelier `--virtgpu-channel`). crosvm then stops the
VM (`--extended-status` exit code 33).

The cross-domain context creates its Wayland read pipe with
`mesa3d_util::create_pipe()`, which calls `rustix::pipe::pipe()`. On x86_64,
rustix implements that with the legacy `pipe` syscall (`__NR_pipe`, 22);
it only uses `pipe2` on architectures without `__NR_pipe` (aarch64, riscv64).
`jail/seccomp/x86_64/gpu_common.policy` allows `pipe2` but not `pipe`, so
the syscall is fatal.

## Versions

- crosvm 37871ccd34e85816928c1b99f4a2a6b5e9404074 (default features, built
  as the AUR `crosvm-git` package), default sandbox
- rutabaga_gfx 0.1.76-alpha.0, mesa3d_util 0.1.76-alpha.0, rustix 1.x
- Host: Arch Linux x86_64, GNOME Wayland session
- Guest: Arch Linux with sommelier `--virtgpu-channel`

## Steps to reproduce

1. `crosvm run --gpu context-types=cross-domain --wayland-sock $XDG_RUNTIME_DIR/wayland-0 ...`
   with sommelier running in the guest.
2. Copy text in a host application.
3. Paste it into a Wayland application in the guest.

## Expected

The text is pasted.

## Actual

The VM stops:

```
ERROR crosvm::crosvm::sys::linux] child pcivirtio-gpu (pid 1093029) killed by signal 31 (Bad system call)
ERROR devices::proxy] failed write to child device process pcivirtio-gpu: failed to send packet: Broken pipe (os error 32)
ERROR devices::proxy] failed to read result of Shutdown from child device process pcivirtio-gpu: tube was disconnected
INFO  crosvm] exiting with crash
```

Pasting works when the clipboard is owned by a guest app, because sommelier
then serves the paste without asking the host, so no read pipe is created.

## Code path

- `rutabaga_gfx/src/cross_domain/context.rs`: for
  `CROSS_DOMAIN_ID_TYPE_READ_PIPE`, `create_pipe()` makes the pipe whose
  write end is sent to the compositor.
- `mesa3d_util/sys/linux/pipe.rs`: `create_pipe()` calls
  `rustix::pipe::pipe()`.
- `rustix/src/backend/linux_raw/pipe/syscalls.rs`: `pipe()` is
  `syscall!(__NR_pipe, ...)` except on aarch64, mips and riscv64.

## Suggested fix

Either of these fixes it:

- In mesa3d_util, use `rustix::pipe::pipe_with(PipeFlags::CLOEXEC)`. That
  is `pipe2` on every architecture, matches the existing policies, and keeps
  the descriptors from leaking into child processes.
- Add `pipe: 1` to `jail/seccomp/x86_64/gpu_common.policy` (and any other
  x86_64 policy whose device can reach `create_pipe()`).

## Workaround

Rebuild crosvm with `pipe: 1` added to the x86_64 GPU policy (the policy is
compiled into the binary), or pass `--seccomp-policy-dir` with a copy of the
policies that allows it. `--disable-sandbox` also avoids the crash.
