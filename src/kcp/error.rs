use std::{
    io,
    sync::{MutexGuard, PoisonError},
};

use super::{ConvAllocator, Kcp};

pub type Result<T> = std::result::Result<T, KcpError>;

#[derive(Debug)]
pub enum KcpError {
    Core(KcpErrorKind),
    NoMoreConv,
    ReadTimeout(u32),
    WriteTimeout(u32),
    WriteError(i32),
    Closed,
    SignalReadClosed,
    SignalSendClosed,
}

#[derive(Debug)]
pub enum KcpErrorKind {
    CreateFail,
    InputError(i32),
    InvalidConv(u32),
    BufferTooSmall(usize, usize),
    InvalidCommand(u32, u8),
    StdIoError(std::io::Error),
}

impl From<KcpErrorKind> for KcpError {
    fn from(value: KcpErrorKind) -> Self {
        Self::Core(value)
    }
}

impl<'a, A> From<PoisonError<MutexGuard<'a, Kcp<A>>>> for KcpError
where
    A: ConvAllocator,
{
    fn from(_value: PoisonError<MutexGuard<'a, Kcp<A>>>) -> Self {
        unimplemented!()
    }
}

impl From<std::io::Error> for KcpError {
    fn from(e: std::io::Error) -> Self {
        Self::Core(KcpErrorKind::StdIoError(e))
    }
}

impl From<KcpError> for io::Error {
    fn from(kcp_err: KcpError) -> Self {
        match kcp_err {
            KcpError::Closed => io::ErrorKind::BrokenPipe.into(),
            KcpError::Core(kind) => kind.into(),
            KcpError::WriteTimeout(_) => io::ErrorKind::TimedOut.into(),
            KcpError::SignalReadClosed => io::ErrorKind::BrokenPipe.into(),
            KcpError::SignalSendClosed => io::ErrorKind::BrokenPipe.into(),
            KcpError::ReadTimeout(_) => io::ErrorKind::TimedOut.into(),
            KcpError::NoMoreConv => io::Error::new(io::ErrorKind::AddrInUse, "conv exhausted"),
            KcpError::WriteError(val) => {
                io::Error::new(io::ErrorKind::Other, format!("write({})", val))
            }
        }
    }
}

impl From<KcpErrorKind> for io::Error {
    fn from(err_kind: KcpErrorKind) -> Self {
        match err_kind {
            KcpErrorKind::CreateFail => io::ErrorKind::OutOfMemory.into(),
            KcpErrorKind::InputError(_) => io::ErrorKind::InvalidInput.into(),
            KcpErrorKind::InvalidConv(_) => {
                io::Error::new(io::ErrorKind::InvalidData, "invalid conv")
            }
            KcpErrorKind::BufferTooSmall(size, need) => {
                io::Error::new(io::ErrorKind::Other, format!("buffer too small: {} {}", size, need))
            }
            KcpErrorKind::InvalidCommand(_, _) => {
                io::Error::new(io::ErrorKind::InvalidData, "invalid header")
            }
            KcpErrorKind::StdIoError(e) => e,
        }
    }
}
