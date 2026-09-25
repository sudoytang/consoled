use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};

use nix::fcntl::{fcntl, FcntlArg, FdFlag};
use nix::unistd::close;

use crate::error::{Error, Result};

pub static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_shutdown(_sig: libc::c_int) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

pub fn install_shutdown_handler() -> Result<()> {
    let handler = nix::sys::signal::SigHandler::Handler(handle_shutdown);
    let action = nix::sys::signal::SigAction::new(
        handler,
        nix::sys::signal::SaFlags::empty(),
        nix::sys::signal::SigSet::empty(),
    );
    unsafe {
        nix::sys::signal::sigaction(nix::sys::signal::Signal::SIGTERM, &action)?;
        nix::sys::signal::sigaction(nix::sys::signal::Signal::SIGINT, &action)?;
        nix::sys::signal::sigaction(nix::sys::signal::Signal::SIGHUP, &action)?;
    }
    let ignore = nix::sys::signal::SigAction::new(
        nix::sys::signal::SigHandler::SigIgn,
        nix::sys::signal::SaFlags::empty(),
        nix::sys::signal::SigSet::empty(),
    );
    unsafe {
        nix::sys::signal::sigaction(nix::sys::signal::Signal::SIGPIPE, &ignore)?;
    }
    Ok(())
}

pub fn shutdown_requested() -> bool {
    SHUTDOWN.load(Ordering::SeqCst)
}

pub fn set_nonblocking(fd: RawFd, on: bool) -> Result<()> {
    let flags = fcntl(fd, FcntlArg::F_GETFL).map_err(Error::from)?;
    let mut flags = nix::fcntl::OFlag::from_bits_truncate(flags);
    if on {
        flags.insert(nix::fcntl::OFlag::O_NONBLOCK);
    } else {
        flags.remove(nix::fcntl::OFlag::O_NONBLOCK);
    }
    fcntl(fd, FcntlArg::F_SETFL(flags))?;
    Ok(())
}

pub fn set_cloexec(fd: RawFd, on: bool) -> Result<()> {
    let flags = fcntl(fd, FcntlArg::F_GETFD).map_err(Error::from)?;
    let mut flags = FdFlag::from_bits_truncate(flags);
    if on {
        flags.insert(FdFlag::FD_CLOEXEC);
    } else {
        flags.remove(FdFlag::FD_CLOEXEC);
    }
    fcntl(fd, FcntlArg::F_SETFD(flags))?;
    Ok(())
}

pub fn close_fds_from(min_fd: RawFd) {
    if min_fd < 0 {
        return;
    }
    for fd in min_fd..=1024 {
        let _ = close(fd);
    }
}

pub fn session_pids(sid: libc::pid_t) -> Vec<libc::pid_t> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ent in dir.flatten() {
        let name = ent.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Ok(pid) = name.parse::<libc::pid_t>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(ent.path().join("stat")) else {
            continue;
        };
        if parse_sid(&stat) == Some(sid) {
            out.push(pid);
        }
    }
    out
}

fn parse_sid(stat: &str) -> Option<libc::pid_t> {
    let rparen = stat.rfind(')')?;
    let rest: Vec<&str> = stat[rparen + 1..].split_whitespace().collect();
    rest.get(3)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::parse_sid;

    #[test]
    fn parse_sid_with_spaces_in_comm() {
        let stat = "10 (some proc) S 1 10 10 0 0";
        assert_eq!(parse_sid(stat), Some(10));
    }
}
