use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;

use nix::unistd::{chroot, setgid, setgroups, setuid, Gid, Uid, User};
use seccompiler::{
    apply_filter, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};

use crate::config::Config;
use crate::error::{Error, Result};

pub fn drop_privileges(cfg: &Config) -> Result<()> {
    if cfg.no_privdrop {
        tracing::warn!("privilege drop disabled (--no-privdrop); debug only");
        if !cfg.no_seccomp {
            apply_seccomp()?;
        }
        return Ok(());
    }

    let user = User::from_name(&cfg.user)
        .map_err(|e| Error::Privilege(e.to_string()))?
        .ok_or_else(|| Error::Privilege(format!("user {} not found", cfg.user)))?;

    let meta = std::fs::metadata(&cfg.chroot).map_err(|e| {
        Error::Privilege(format!(
            "chroot {} is not accessible: {e}",
            cfg.chroot.display()
        ))
    })?;
    if !meta.is_dir() {
        return Err(Error::Privilege("chroot path is not a directory".into()));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(Error::Privilege(
            "chroot directory must not be group/world writable".into(),
        ));
    }

    chroot(&cfg.chroot).map_err(|e| Error::Privilege(format!("chroot failed: {e}")))?;
    nix::unistd::chdir("/").map_err(|e| Error::Privilege(format!("chdir / failed: {e}")))?;

    setgroups(&[Gid::from_raw(user.gid.as_raw())])
        .map_err(|e| Error::Privilege(format!("setgroups failed: {e}")))?;
    setgid(user.gid).map_err(|e| Error::Privilege(format!("setgid failed: {e}")))?;
    setuid(user.uid).map_err(|e| Error::Privilege(format!("setuid failed: {e}")))?;

    if nix::unistd::geteuid() == Uid::from_raw(0) {
        return Err(Error::Privilege("still root after setuid".into()));
    }

    set_no_new_privs()?;

    if !cfg.no_seccomp {
        apply_seccomp()?;
    }
    Ok(())
}

fn set_no_new_privs() -> Result<()> {
    let rc = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if rc != 0 {
        return Err(Error::Privilege(format!(
            "PR_SET_NO_NEW_PRIVS failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

fn target_arch() -> Result<TargetArch> {
    #[cfg(target_arch = "x86_64")]
    {
        return Ok(TargetArch::x86_64);
    }
    #[cfg(target_arch = "aarch64")]
    {
        return Ok(TargetArch::aarch64);
    }
    #[allow(unreachable_code)]
    Err(Error::Privilege(
        "seccomp filter is only defined for x86_64 and aarch64".into(),
    ))
}

fn allow(sys: i64) -> (i64, Vec<SeccompRule>) {
    (sys, Vec::new())
}

/// Stop glibc malloc from reading `/sys/devices/system/cpu/online`.
///
/// When a process has more threads than `arena_test` (8 on 64-bit) and
/// needs another arena, glibc calls `__get_nprocs()`, which `openat`s that
/// file. `openat` is not in the seccomp allowlist, so the child would be
/// killed with SIGSYS at a timing-dependent point (e.g. when tokio's
/// blocking pool grows). Fixing `M_ARENA_MAX` up front skips that path.
fn cap_malloc_arenas() {
    #[cfg(target_env = "gnu")]
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, 4);
    }
}

fn apply_seccomp() -> Result<()> {
    cap_malloc_arenas();
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = [
        allow(libc::SYS_read),
        allow(libc::SYS_write),
        allow(libc::SYS_readv),
        allow(libc::SYS_writev),
        allow(libc::SYS_pread64),
        allow(libc::SYS_pwrite64),
        allow(libc::SYS_close),
        allow(libc::SYS_lseek),
        allow(libc::SYS_fstat),
        allow(libc::SYS_newfstatat),
        allow(libc::SYS_statx),
        allow(libc::SYS_poll),
        allow(libc::SYS_ppoll),
        allow(libc::SYS_epoll_create1),
        allow(libc::SYS_epoll_ctl),
        allow(libc::SYS_epoll_wait),
        allow(libc::SYS_epoll_pwait),
        allow(libc::SYS_recvfrom),
        allow(libc::SYS_sendto),
        allow(libc::SYS_recvmsg),
        allow(libc::SYS_sendmsg),
        allow(libc::SYS_shutdown),
        allow(libc::SYS_getsockopt),
        allow(libc::SYS_setsockopt),
        allow(libc::SYS_getsockname),
        allow(libc::SYS_getpeername),
        allow(libc::SYS_mmap),
        allow(libc::SYS_munmap),
        allow(libc::SYS_mprotect),
        allow(libc::SYS_mremap),
        allow(libc::SYS_brk),
        allow(libc::SYS_madvise),
        allow(libc::SYS_futex),
        allow(libc::SYS_set_robust_list),
        allow(libc::SYS_get_robust_list),
        allow(libc::SYS_clone),
        allow(libc::SYS_clone3),
        allow(libc::SYS_set_tid_address),
        allow(libc::SYS_sched_yield),
        allow(libc::SYS_sched_getaffinity),
        allow(libc::SYS_nanosleep),
        allow(libc::SYS_clock_nanosleep),
        allow(libc::SYS_clock_gettime),
        allow(libc::SYS_clock_getres),
        allow(libc::SYS_gettimeofday),
        allow(libc::SYS_rt_sigaction),
        allow(libc::SYS_rt_sigprocmask),
        allow(libc::SYS_rt_sigreturn),
        allow(libc::SYS_rt_sigtimedwait),
        allow(libc::SYS_sigaltstack),
        allow(libc::SYS_getrandom),
        allow(libc::SYS_getpid),
        allow(libc::SYS_gettid),
        allow(libc::SYS_getuid),
        allow(libc::SYS_geteuid),
        allow(libc::SYS_getgid),
        allow(libc::SYS_getegid),
        allow(libc::SYS_getppid),
        allow(libc::SYS_fcntl),
        allow(libc::SYS_eventfd2),
        allow(libc::SYS_pipe2),
        allow(libc::SYS_exit),
        allow(libc::SYS_exit_group),
        allow(libc::SYS_prctl),
        allow(libc::SYS_restart_syscall),
        allow(libc::SYS_rseq),
        allow(libc::SYS_membarrier),
        allow(libc::SYS_mincore),
        allow(libc::SYS_getrusage),
        allow(libc::SYS_getrlimit),
        allow(libc::SYS_prlimit64),
        allow(libc::SYS_capget),
        allow(libc::SYS_tgkill),
        allow(libc::SYS_sched_getscheduler),
        allow(libc::SYS_sched_getparam),
    ]
    .into_iter()
    .collect();

    #[cfg(target_arch = "x86_64")]
    {
        rules.insert(libc::SYS_epoll_create, Vec::new());
        rules.insert(libc::SYS_epoll_pwait2, Vec::new());
        rules.insert(libc::SYS_futex_waitv, Vec::new());
        rules.insert(libc::SYS_close_range, Vec::new());
    }

    let ioctl_rules = ioctl_allowlist()?;
    rules.insert(libc::SYS_ioctl, ioctl_rules);

    let filter = SeccompFilter::new(
        rules,
        SeccompAction::KillProcess,
        SeccompAction::Allow,
        target_arch()?,
    )
    .map_err(|e| Error::Privilege(format!("seccomp filter: {e:?}")))?;
    let program: seccompiler::BpfProgram = filter
        .try_into()
        .map_err(|e| Error::Privilege(format!("seccomp compile: {e:?}")))?;
    apply_filter(&program).map_err(|e| Error::Privilege(format!("seccomp apply: {e:?}")))?;
    Ok(())
}

fn ioctl_allowlist() -> Result<Vec<SeccompRule>> {
    let reqs = [
        libc::TIOCSWINSZ,
        libc::FIONBIO,
        libc::FIONREAD,
        libc::TCGETS,
        libc::TCSETS,
    ];
    let mut out = Vec::new();
    for req in reqs {
        let cond = SeccompCondition::new(1, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, req)
            .map_err(|e| Error::Privilege(format!("seccomp ioctl cond: {e:?}")))?;
        let rule = SeccompRule::new(vec![cond])
            .map_err(|e| Error::Privilege(format!("seccomp ioctl rule: {e:?}")))?;
        out.push(rule);
    }
    Ok(out)
}
