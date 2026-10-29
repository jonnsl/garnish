//! A seccomp filter that allows only what's left to do once every file and
//! socket is open: no new files, sockets or processes, and the calls that
//! act on a descriptor only on the descriptors meant for them.

use std::io;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("maitre-net's seccomp filter knows only x86_64 and aarch64");

// Offsets in struct seccomp_data.
const NR: u32 = 0;
const ARCH: u32 = 4;
/// The low 32 bits of argument `i` (little-endian). Descriptors and ioctl
/// requests are ints: the kernel ignores the high bits, so we do too.
const fn arg(i: u32) -> u32 {
    16 + 8 * i
}

/// A system call and what it may be called with.
pub struct Rule {
    pub nr: libc::c_long,
    /// The descriptor in the first argument, if it matters.
    pub fd: Option<i32>,
    /// The allowed values of the second argument (ioctl requests, fcntl
    /// commands); empty for any.
    pub arg1: Vec<u32>,
}

impl Rule {
    pub fn any(nr: libc::c_long) -> Rule {
        Rule { nr, fd: None, arg1: Vec::new() }
    }

    pub fn on(nr: libc::c_long, fd: i32) -> Rule {
        Rule { nr, fd: Some(fd), arg1: Vec::new() }
    }

    pub fn ioctl(fd: i32, requests: &[libc::c_ulong]) -> Rule {
        Rule { nr: libc::SYS_ioctl, fd: Some(fd), arg1: requests.iter().map(|r| *r as u32).collect() }
    }

    /// Only with this second argument, on any descriptor.
    pub fn with(nr: libc::c_long, arg1: u32) -> Rule {
        Rule { nr, fd: None, arg1: vec![arg1] }
    }
}

fn stmt(code: u32, k: u32) -> libc::sock_filter {
    libc::sock_filter { code: code as u16, jt: 0, jf: 0, k }
}

fn jeq(k: u32, jt: usize, jf: usize) -> libc::sock_filter {
    let jump = |n: usize| u8::try_from(n).expect("seccomp jump too long");
    libc::sock_filter { code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16, jt: jump(jt), jf: jump(jf), k }
}

fn load(offset: u32) -> libc::sock_filter {
    stmt(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, offset)
}

fn ret(action: u32) -> libc::sock_filter {
    stmt(libc::BPF_RET | libc::BPF_K, action)
}

/// One rule, self-contained: it allows the call or falls through to the
/// next rule.
fn rule_program(rule: &Rule) -> Vec<libc::sock_filter> {
    let allow = ret(libc::SECCOMP_RET_ALLOW);
    let mut tail = Vec::new();
    if rule.arg1.is_empty() {
        tail.push(allow);
    } else {
        tail.push(load(arg(1)));
        for &value in &rule.arg1 {
            tail.push(jeq(value, 0, 1));
            tail.push(allow);
        }
    }
    if let Some(fd) = rule.fd {
        let rest = tail.len();
        tail.splice(0..0, [load(arg(0)), jeq(fd as u32, 0, rest)]);
    }
    let rest = tail.len();
    let mut program = vec![load(NR), jeq(rule.nr as u32, 0, rest)];
    program.extend(tail);
    program
}

pub fn program(rules: &[Rule]) -> Vec<libc::sock_filter> {
    let kill = ret(libc::SECCOMP_RET_KILL_PROCESS);
    // A call for another architecture (x86_64 can also make i386 calls)
    // has other numbers.
    let mut program = vec![load(ARCH), jeq(AUDIT_ARCH, 1, 0), kill];
    #[cfg(target_arch = "x86_64")]
    {
        // x32 calls are x86_64 calls with this bit set.
        const X32_SYSCALL_BIT: u32 = 0x4000_0000;
        program.push(load(NR));
        program.push(libc::sock_filter {
            code: (libc::BPF_JMP | libc::BPF_JGE | libc::BPF_K) as u16,
            jt: 0,
            jf: 1,
            k: X32_SYSCALL_BIT,
        });
        program.push(kill);
    }
    for rule in rules {
        program.extend(rule_program(rule));
    }
    program.push(kill);
    program
}

/// Installs the filter for this (single-threaded) process. Needs
/// no_new_privs.
pub fn install(rules: &[Rule]) -> Result<(), String> {
    let program = program(rules);
    let fprog = libc::sock_fprog { len: program.len() as u16, filter: program.as_ptr().cast_mut() };
    let ret = unsafe {
        libc::syscall(libc::SYS_seccomp, libc::SECCOMP_SET_MODE_FILTER, 0 as libc::c_ulong, &fprog as *const _)
    };
    if ret != 0 {
        return Err(format!("installing the seccomp filter: {}", io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the filter like the kernel would, for one call.
    fn run(program: &[libc::sock_filter], nr: libc::c_long, args: [u64; 6]) -> u32 {
        let mut data = [0u8; 64];
        data[0..4].copy_from_slice(&(nr as u32).to_le_bytes());
        data[4..8].copy_from_slice(&AUDIT_ARCH.to_le_bytes());
        for (i, a) in args.iter().enumerate() {
            data[16 + 8 * i..24 + 8 * i].copy_from_slice(&a.to_le_bytes());
        }
        let mut acc = 0u32;
        let mut pc = 0;
        loop {
            let insn = program[pc];
            let code = u32::from(insn.code);
            pc += 1;
            if code == libc::BPF_LD | libc::BPF_W | libc::BPF_ABS {
                let k = insn.k as usize;
                acc = u32::from_le_bytes(data[k..k + 4].try_into().unwrap());
            } else if code == libc::BPF_RET | libc::BPF_K {
                return insn.k;
            } else if code == libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K {
                pc += usize::from(if acc == insn.k { insn.jt } else { insn.jf });
            } else if code == libc::BPF_JMP | libc::BPF_JGE | libc::BPF_K {
                pc += usize::from(if acc >= insn.k { insn.jt } else { insn.jf });
            } else {
                panic!("unexpected instruction {code:#x}");
            }
        }
    }

    #[test]
    fn filter() {
        let p = program(&[
            Rule::any(libc::SYS_close),
            Rule::on(libc::SYS_write, 2),
            Rule::ioctl(5, &[libc::TUNSETIFF as libc::c_ulong, 0x8933]),
        ]);
        let allow = libc::SECCOMP_RET_ALLOW;
        let kill = libc::SECCOMP_RET_KILL_PROCESS;
        assert_eq!(run(&p, libc::SYS_close, [9, 0, 0, 0, 0, 0]), allow);
        assert_eq!(run(&p, libc::SYS_write, [2, 0, 0, 0, 0, 0]), allow);
        assert_eq!(run(&p, libc::SYS_write, [3, 0, 0, 0, 0, 0]), kill);
        assert_eq!(run(&p, libc::SYS_ioctl, [5, 0x8933, 0, 0, 0, 0]), allow);
        assert_eq!(run(&p, libc::SYS_ioctl, [5, libc::TUNSETIFF, 0, 0, 0, 0]), allow);
        assert_eq!(run(&p, libc::SYS_ioctl, [5, 0x8934, 0, 0, 0, 0]), kill);
        assert_eq!(run(&p, libc::SYS_ioctl, [4, 0x8933, 0, 0, 0, 0]), kill);
        assert_eq!(run(&p, libc::SYS_openat, [0; 6]), kill);
        #[cfg(target_arch = "x86_64")]
        assert_eq!(run(&p, libc::SYS_close | 0x4000_0000, [0; 6]), kill);
    }
}
