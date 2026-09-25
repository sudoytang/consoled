use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

use nix::sys::socket::{recvmsg, sendmsg, ControlMessage, ControlMessageOwned, MsgFlags};

use crate::error::{Error, Result};
use crate::protocol::Resize;

pub const MAGIC: u32 = 0x434F_4E53; // CONS
pub const VERSION: u32 = 1;
pub const REQ_SIZE: usize = 128;
pub const REP_SIZE: usize = 32;

#[derive(Debug, Clone)]
pub struct StartSessionRequest {
    pub resize: Resize,
    pub client_ip: String,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartStatus {
    Ok = 0,
    Failed = 1,
}

pub fn encode_request(req: &StartSessionRequest) -> Result<[u8; REQ_SIZE]> {
    if req.client_ip.len() > 63 {
        return Err(Error::msg("client ip too long"));
    }
    let mut buf = [0u8; REQ_SIZE];
    buf[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    buf[4..8].copy_from_slice(&VERSION.to_le_bytes());
    buf[8..12].copy_from_slice(&u32::from(req.resize.cols).to_le_bytes());
    buf[12..16].copy_from_slice(&u32::from(req.resize.rows).to_le_bytes());
    let ip = req.client_ip.as_bytes();
    buf[16..20].copy_from_slice(&(ip.len() as u32).to_le_bytes());
    buf[20..20 + ip.len()].copy_from_slice(ip);
    Ok(buf)
}

pub fn decode_request(buf: &[u8]) -> Result<StartSessionRequest> {
    if buf.len() != REQ_SIZE {
        return Err(Error::Protocol("bad request size"));
    }
    let magic = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    let version = u32::from_le_bytes(buf[4..8].try_into().unwrap());
    if magic != MAGIC || version != VERSION {
        return Err(Error::Protocol("bad request magic/version"));
    }
    let cols = u32::from_le_bytes(buf[8..12].try_into().unwrap());
    let rows = u32::from_le_bytes(buf[12..16].try_into().unwrap());
    let ip_len = u32::from_le_bytes(buf[16..20].try_into().unwrap()) as usize;
    if ip_len > 63 {
        return Err(Error::Protocol("bad ip length"));
    }
    let ip = std::str::from_utf8(&buf[20..20 + ip_len])
        .map_err(|_| Error::Protocol("ip is not utf-8"))?;
    let cols = u16::try_from(cols).map_err(|_| Error::Protocol("cols out of range"))?;
    let rows = u16::try_from(rows).map_err(|_| Error::Protocol("rows out of range"))?;
    Ok(StartSessionRequest {
        resize: Resize { cols, rows },
        client_ip: ip.to_string(),
    })
}

fn encode_reply(status: StartStatus) -> [u8; REP_SIZE] {
    let mut buf = [0u8; REP_SIZE];
    buf[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    buf[4..8].copy_from_slice(&VERSION.to_le_bytes());
    buf[8..12].copy_from_slice(&(status as u32).to_le_bytes());
    buf
}

fn decode_reply(buf: &[u8]) -> Result<StartStatus> {
    if buf.len() != REP_SIZE {
        return Err(Error::Protocol("bad reply size"));
    }
    let magic = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    let version = u32::from_le_bytes(buf[4..8].try_into().unwrap());
    if magic != MAGIC || version != VERSION {
        return Err(Error::Protocol("bad reply magic/version"));
    }
    match u32::from_le_bytes(buf[8..12].try_into().unwrap()) {
        0 => Ok(StartStatus::Ok),
        _ => Ok(StartStatus::Failed),
    }
}

pub fn write_all(stream: &mut UnixStream, buf: &[u8]) -> Result<()> {
    stream.write_all(buf)?;
    Ok(())
}

pub fn read_exact(stream: &mut UnixStream, buf: &mut [u8]) -> Result<()> {
    stream.read_exact(buf)?;
    Ok(())
}

pub fn send_request(stream: &mut UnixStream, req: &StartSessionRequest) -> Result<()> {
    let buf = encode_request(req)?;
    write_all(stream, &buf)
}

pub fn recv_request(stream: &mut UnixStream) -> Result<StartSessionRequest> {
    let mut buf = [0u8; REQ_SIZE];
    read_exact(stream, &mut buf)?;
    decode_request(&buf)
}

pub fn send_reply_with_fd(
    stream: &UnixStream,
    status: StartStatus,
    fd: Option<RawFd>,
) -> Result<()> {
    let buf = encode_reply(status);
    let iov = [std::io::IoSlice::new(&buf)];
    if let Some(fd) = fd {
        let fds = [fd];
        let cmsgs = [ControlMessage::ScmRights(&fds)];
        sendmsg::<()>(stream.as_raw_fd(), &iov, &cmsgs, MsgFlags::empty(), None)?;
    } else {
        sendmsg::<()>(stream.as_raw_fd(), &iov, &[], MsgFlags::empty(), None)?;
    }
    Ok(())
}

pub fn recv_reply_with_fd(stream: &UnixStream) -> Result<(StartStatus, Option<OwnedFd>)> {
    let mut buf = [0u8; REP_SIZE];
    let mut raw_fds: Vec<RawFd> = Vec::new();
    let nbytes = {
        let mut iov = [std::io::IoSliceMut::new(&mut buf)];
        let mut cmsg = nix::cmsg_space!(RawFd);
        let msg = recvmsg::<()>(
            stream.as_raw_fd(),
            &mut iov,
            Some(&mut cmsg),
            MsgFlags::empty(),
        )?;
        if let Ok(cmsgs) = msg.cmsgs() {
            for c in cmsgs {
                if let ControlMessageOwned::ScmRights(fds) = c {
                    raw_fds.extend(fds.into_iter().filter(|fd| *fd >= 0));
                }
            }
        }
        msg.bytes
    };
    if nbytes != REP_SIZE {
        return Err(Error::Protocol("short reply"));
    }
    let mut passed = None;
    for raw in raw_fds {
        let owned = unsafe { OwnedFd::from_raw_fd(raw) };
        if passed.is_some() {
            drop(owned);
        } else {
            passed = Some(owned);
        }
    }
    Ok((decode_reply(&buf)?, passed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let req = StartSessionRequest {
            resize: Resize {
                cols: 120,
                rows: 40,
            },
            client_ip: "2001:db8::1".into(),
        };
        let buf = encode_request(&req).unwrap();
        let out = decode_request(&buf).unwrap();
        assert_eq!(out.resize.cols, 120);
        assert_eq!(out.resize.rows, 40);
        assert_eq!(out.client_ip, "2001:db8::1");
    }

    #[test]
    fn request_rejects_bad_magic() {
        let mut buf = encode_request(&StartSessionRequest {
            resize: Resize { cols: 80, rows: 24 },
            client_ip: "127.0.0.1".into(),
        })
        .unwrap();
        buf[0] = 0;
        assert!(decode_request(&buf).is_err());
    }
}
