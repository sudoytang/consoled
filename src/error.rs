use std::fmt;
use std::io;

#[derive(Debug)]
pub enum Error {
    Message(String),
    Io(io::Error),
    Nix(nix::Error),
    Tls(rustls::Error),
    Protocol(&'static str),
    Privilege(String),
}

impl Error {
    pub fn msg(m: impl Into<String>) -> Self {
        Self::Message(m.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message(m) => write!(f, "{m}"),
            Self::Io(e) => write!(f, "{e}"),
            Self::Nix(e) => write!(f, "{e}"),
            Self::Tls(e) => write!(f, "{e}"),
            Self::Protocol(m) => write!(f, "protocol error: {m}"),
            Self::Privilege(m) => write!(f, "privilege error: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<nix::Error> for Error {
    fn from(value: nix::Error) -> Self {
        Self::Nix(value)
    }
}

impl From<rustls::Error> for Error {
    fn from(value: rustls::Error) -> Self {
        Self::Tls(value)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
