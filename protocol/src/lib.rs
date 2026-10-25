//! Portable native protocol core, independent of Python, FFmpeg and a window.
//! Apple-specific framing is checked against generated reference fixtures.
pub mod auth;
pub mod control;
mod ffi;
pub mod hevc;
pub mod srtp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidKey,
    InvalidPacket,
    Authentication,
    SourceLimit,
    CounterExhausted,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
