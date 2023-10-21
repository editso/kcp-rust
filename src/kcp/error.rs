use std::{sync::{MutexGuard, PoisonError}, convert::Infallible};

use crate::signal;

use super::{ConvAllocator, Kcp};

pub type Result<T> = std::result::Result<T, KcpError>;

#[derive(Debug)]
pub enum KcpError {
    Core(KcpErrorKind),
    SignalReadClosed,
    SignalSendClosed
}

#[derive(Debug)]
pub enum KcpErrorKind {
    CreateFail,
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
    fn from(value: PoisonError<MutexGuard<'a, Kcp<A>>>) -> Self {
        unimplemented!()
    }
}

impl From<std::io::Error> for KcpError {
    fn from(e: std::io::Error) -> Self {
        Self::Core(KcpErrorKind::StdIoError(e))
    }
}
