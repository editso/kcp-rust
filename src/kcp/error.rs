pub type Result<T> = std::result::Result<T, KcpError>;


#[derive(Debug)]
pub enum KcpError{
  Core(KcpErrorKind)
}


#[derive(Debug)]
pub enum KcpErrorKind{
  CreateFail,
  InvalidConv(u32),
  UserDataToSmall(u32),
  InvalidCommand(u32, u8),
}

impl From<KcpErrorKind> for KcpError{
  fn from(value: KcpErrorKind) -> Self {
      Self::Core(value)
  }
}