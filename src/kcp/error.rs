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
    Closed,
    SignalReadClosed,
    SignalSendClosed,
}

#[derive(Debug)]
pub enum KcpErrorKind {
    CreateFail,
    InputError(i32),
    InvalidConv(u32),
    UserDataToSmall(u32),
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
            KcpError::Core(_coew) => todo!(),
            KcpError::WriteTimeout(_) => todo!(),
            KcpError::SignalReadClosed => todo!(),
            KcpError::SignalSendClosed => todo!(),
            KcpError::Closed => todo!(),
            KcpError::ReadTimeout(_) => todo!(),
            KcpError::NoMoreConv => todo!(),
        }
    }
}
