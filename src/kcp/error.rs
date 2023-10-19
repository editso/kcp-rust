use std::sync::{PoisonError, MutexGuard};

use super::{Kcp, ConvAllocator};

pub type Result<T> = std::result::Result<T, KcpError>;

#[derive(Debug)]
pub enum KcpError {
    Core(KcpErrorKind),
}

#[derive(Debug)]
pub enum KcpErrorKind {
    CreateFail,
    InvalidConv(u32),
    UserDataToSmall(u32),
    InvalidCommand(u32, u8),
}

impl From<KcpErrorKind> for KcpError {
    fn from(value: KcpErrorKind) -> Self {
        Self::Core(value)
    }
}

impl<'a, A> From<PoisonError<MutexGuard<'a, Kcp<A>>>> for KcpError 
where A: ConvAllocator{
   fn from(value: PoisonError<MutexGuard<'a, Kcp<A>>>) -> Self {
       unimplemented!()
   }
}
