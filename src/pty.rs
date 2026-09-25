use std::ffi::CString;
use std::os::fd::{AsRawFd, OwnedFd};

use nix::pty::{openpty, OpenptyResult, Winsize};
use nix::unistd::{execve, fork, ForkResult, Pid};

use crate::error::{Error, Result};
use crate::linux::close_fds_from;
use crate::protocol::Resize;

/// PTY master plus the login session leader pid (session id).
///
/// This type has no WebSocket or HTTP concepts. The caller is responsible for
/// I/O on the master and for process cleanup.
pub struct LoginPty {
    pub master: OwnedFd,
    pub session_pid: Pid,
}

pub fn set_winsize(fd: i32, resize: Resize) -> Result<()> {
    let ws = libc::winsize {
        ws_row: resize.rows,
        ws_col: resize.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) };
    if rc != 0 {
        return Err(Error::from(std::io::Error::last_os_error()));
    }
    Ok(())
}

/// Allocate a PTY and exec `login -h <client_ip>` as the session leader
/// with the slave as controlling tty and stdin/stdout/stderr.
///
/// `login` performs PAM authentication. This function does not read
/// credentials or interpret terminal bytes.
pub fn spawn_login(
    login_path: &std::path::Path,
    resize: Resize,
    client_ip: &str,
) -> Result<LoginPty> {
    if !login_path.is_absolute() {
        return Err(Error::msg("login path must be absolute"));
    }
    let winsize = Winsize {
        ws_row: resize.rows,
        ws_col: resize.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let OpenptyResult { master, slave } = openpty(Some(&winsize), None)?;

    let login_c = CString::new(login_path.to_string_lossy().as_bytes())
        .map_err(|_| Error::msg("login path contains NUL"))?;
    let arg0 = CString::new("login").unwrap();
    let argh = CString::new("-h").unwrap();
    let argip = CString::new(client_ip).map_err(|_| Error::msg("client ip contains NUL"))?;
    let env_term = CString::new("TERM=xterm-256color").unwrap();
    let env_path =
        CString::new("PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin").unwrap();
    let env_lang = CString::new("LANG=C.UTF-8").unwrap();

    match unsafe { fork() }? {
        ForkResult::Parent { child } => {
            drop(slave);
            Ok(LoginPty {
                master,
                session_pid: child,
            })
        }
        ForkResult::Child => {
            drop(master);
            if let Err(e) = become_session_and_exec(
                slave,
                &login_c,
                &[&arg0, &argh, &argip],
                &[&env_term, &env_path, &env_lang],
            ) {
                let msg = format!("consoled: exec login failed: {e}\n");
                unsafe {
                    libc::write(2, msg.as_ptr().cast(), msg.len());
                }
            }
            unsafe { libc::_exit(127) };
        }
    }
}

fn become_session_and_exec(
    slave: OwnedFd,
    login: &CString,
    args: &[&CString],
    env: &[&CString],
) -> Result<()> {
    nix::unistd::setsid()?;
    let rc = unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCSCTTY, 0) };
    if rc != 0 {
        return Err(Error::from(std::io::Error::last_os_error()));
    }
    let slave_fd = slave.as_raw_fd();
    if unsafe { libc::dup2(slave_fd, 0) } < 0
        || unsafe { libc::dup2(slave_fd, 1) } < 0
        || unsafe { libc::dup2(slave_fd, 2) } < 0
    {
        return Err(Error::from(std::io::Error::last_os_error()));
    }
    drop(slave);
    close_fds_from(3);
    execve(login, args, env)?;
    Ok(())
}
