//! gerant-boot: finds the kernel to boot in a VM's disk image, given on
//! standard input, and sends it and its initrd to standard output for
//! gerantd (see `gerant::boot`). With `--list`, it lists the kernels it
//! finds instead, the one it would boot first.
//!
//! Everything in the image comes from the guest, so before reading any of
//! it, gerant-boot
//! 1. closes every descriptor but standard input, output and error,
//! 2. limits its memory and CPU time,
//! 3. installs a seccomp filter that allows reading standard input,
//!    writing standard output and error, managing memory and exiting, and
//!    nothing else: no opening files, no sockets, no new processes.
//!
//! It exits with 0 when it sent a kernel, 2 when the image has none, and 1
//! on errors, with the reason as the last line on standard error. Earlier
//! lines are warnings about kernels it skipped.

use std::fs::File;
use std::io::{self, Write};
use std::mem::ManuallyDrop;
use std::os::fd::FromRawFd;
use std::process::ExitCode;

use gerant::boot::{self, entries::Disk};
use gerant::seccomp::{self, Rule};

fn check(what: &str, ret: libc::c_long) -> Result<(), String> {
    if ret < 0 {
        Err(format!("{what}: {}", io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

/// Locks us in; returns the size of the disk on standard input.
fn sandbox() -> Result<u64, String> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    check("fstat", unsafe { libc::fstat(0, &mut st) }.into())?;
    let kind = st.st_mode & libc::S_IFMT;
    if kind != libc::S_IFREG && kind != libc::S_IFBLK {
        return Err("standard input has to be the disk image (a file or block device)".into());
    }
    let size = unsafe { libc::lseek(0, 0, libc::SEEK_END) };
    check("lseek", size)?;

    check("close_range", unsafe {
        libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32)
    })?;
    let limits = [
        // The kernel and initrd go through in 1 MiB pieces; the largest
        // allocation is a cluster chain (4 bytes per cluster).
        (libc::RLIMIT_AS, 512 << 20),
        (libc::RLIMIT_CPU, 30),
        (libc::RLIMIT_CORE, 0),
        (libc::RLIMIT_NPROC, 0),
    ];
    for (resource, value) in limits {
        let limit = libc::rlimit {
            rlim_cur: value,
            rlim_max: value,
        };
        check(
            "setrlimit",
            unsafe { libc::setrlimit(resource, &limit) }.into(),
        )?;
    }
    for (what, option) in [
        ("PR_SET_DUMPABLE", libc::PR_SET_DUMPABLE),
        ("PR_SET_NO_NEW_PRIVS", libc::PR_SET_NO_NEW_PRIVS),
    ] {
        let value: libc::c_ulong = if option == libc::PR_SET_DUMPABLE {
            0
        } else {
            1
        };
        check(what, unsafe { libc::prctl(option, value, 0, 0, 0) }.into())?;
    }
    seccomp::install(&[
        Rule::on(libc::SYS_pread64, 0),
        Rule::on(libc::SYS_write, 1),
        Rule::on(libc::SYS_write, 2),
        Rule::any(libc::SYS_brk),
        Rule::any(libc::SYS_mmap),
        Rule::any(libc::SYS_munmap),
        Rule::any(libc::SYS_mremap),
        Rule::any(libc::SYS_madvise),
        Rule::any(libc::SYS_futex),
        Rule::any(libc::SYS_rt_sigprocmask),
        Rule::any(libc::SYS_sigaltstack),
        Rule::any(libc::SYS_exit),
        Rule::any(libc::SYS_exit_group),
    ])?;
    Ok(size as u64)
}

fn run(list: bool, size: u64) -> Result<bool, String> {
    // Standard input stays open: we don't own it.
    let disk = ManuallyDrop::new(unsafe { File::from_raw_fd(0) });
    let mut warnings = Vec::new();
    let found = Disk::open(&*disk, size, &mut warnings)?;
    let plans = found.plans(&mut warnings);
    for w in &warnings {
        eprintln!("{w}");
    }
    let Some(first) = plans.first() else {
        return Ok(false);
    };
    let stdout = io::stdout();
    let mut out = stdout.lock();
    if list {
        for p in &plans {
            let _ = writeln!(
                out,
                "{}\t{}\tkernel {} KiB, initrd {} KiB{}",
                p.name,
                p.version.as_deref().unwrap_or("-"),
                p.kernel.len >> 10,
                p.initrd_len() >> 10,
                if p.bad { ", no tries left" } else { "" }
            );
        }
        return Ok(true);
    }
    boot::send(first, &mut out)?;
    Ok(true)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let list = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        [] => false,
        ["--list"] => true,
        _ => {
            eprintln!("usage: {} [--list] < disk.raw", boot::HELPER);
            return ExitCode::FAILURE;
        }
    };
    let result = sandbox().and_then(|size| run(list, size));
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(boot::EXIT_NONE as u8),
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
