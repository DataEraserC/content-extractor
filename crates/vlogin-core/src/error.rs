//! Error type shared across the core. `Channel` errors carry the structured
//! "reserved but not implemented" contract hosts render as-is; everything
//! else is a plain message.
//!
//! `Io` exists for the wire contract, not because this crate performs I/O: the
//! ABI maps it onto the stable `kind: "io"` string a host branches on, so the
//! variant has to survive the move of every filesystem call out of here. It
//! carries a message rather than a `std::io::Error` for the same reason — a
//! protocol layer does not own an effect's error type. `Display` is unchanged,
//! so the text a host sees for an I/O failure is exactly what it was.

use std::fmt;

#[derive(Debug)]
pub enum Error {
    Io(String),
    Json(serde_json::Error),
    /// A channel/method contract problem: unknown channel, reserved method,
    /// bad parameters. Hosts surface these verbatim.
    Channel(String),
    /// Cookie codec/storage problem.
    Cookie(String),
    /// Transport or upstream API problem.
    Http(String),
    /// Anything else.
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn io(message: impl Into<String>) -> Self {
        Error::Io(message.into())
    }
    pub fn channel(message: impl Into<String>) -> Self {
        Error::Channel(message.into())
    }
    pub fn cookie(message: impl Into<String>) -> Self {
        Error::Cookie(message.into())
    }
    pub fn http(message: impl Into<String>) -> Self {
        Error::Http(message.into())
    }
    pub fn other(message: impl Into<String>) -> Self {
        Error::Other(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(message) => write!(f, "{message}"),
            Error::Json(e) => write!(f, "{e}"),
            Error::Channel(m) | Error::Cookie(m) | Error::Http(m) | Error::Other(m) => {
                write!(f, "{m}")
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}
