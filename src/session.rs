use std::time::{Duration, Instant};

use nix::sys::signal::{kill, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;

use crate::config::Config;
use crate::linux::session_pids;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    WsClosed,
    PtyEof,
    ResizeTimeout,
    ProtocolError,
    MessageTooBig,
    SessionSetupFailed,
    IdleTimeout,
    LifetimeExceeded,
    ChildError,
    #[allow(dead_code)]
    ServerShutdown,
    HttpOnly,
    #[allow(dead_code)]
    LimitDrop,
}

impl ExitReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WsClosed => "ws_closed",
            Self::PtyEof => "pty_eof",
            Self::ResizeTimeout => "resize_timeout",
            Self::ProtocolError => "protocol_error",
            Self::MessageTooBig => "message_too_big",
            Self::SessionSetupFailed => "session_setup_failed",
            Self::IdleTimeout => "idle_timeout",
            Self::LifetimeExceeded => "lifetime_exceeded",
            Self::ChildError => "child_error",
            Self::ServerShutdown => "server_shutdown",
            Self::HttpOnly => "http_only",
            Self::LimitDrop => "limit_drop",
        }
    }
}

/// sshd-style disconnect cleanup: SIGHUP the session, wait, SIGTERM, wait, SIGKILL.
///
/// Processes that have called setsid/nohup/tmux (new session id) are left alone.
pub fn cleanup_session(sid: Pid, cfg: &Config) {
    escalate(sid, Signal::SIGHUP);
    if wait_session_gone(sid, cfg.cleanup_hup_timeout) {
        reap_zombies();
        return;
    }
    escalate(sid, Signal::SIGTERM);
    if wait_session_gone(sid, cfg.cleanup_term_timeout) {
        reap_zombies();
        return;
    }
    escalate(sid, Signal::SIGKILL);
    let _ = wait_session_gone(sid, Duration::from_secs(2));
    reap_zombies();
}

fn escalate(sid: Pid, sig: Signal) {
    for pid in session_pids(sid.as_raw()) {
        let _ = kill(Pid::from_raw(pid), sig);
    }
}

fn wait_session_gone(sid: Pid, timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        reap_zombies();
        let left = session_pids(sid.as_raw());
        if left.is_empty() {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn reap_zombies() {
    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) | Err(_) => break,
            Ok(_) => continue,
        }
    }
}

pub fn waitpid_nonblock(pid: Pid) -> Option<WaitStatus> {
    match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
        Ok(WaitStatus::StillAlive) => None,
        Ok(st) => Some(st),
        Err(_) => None,
    }
}
