mod bindings;
mod error;

use bindings::*;

pub use error::*;
pub use ikcp::{CB, CONV_T};

#[derive(Debug)]
pub struct User {
    user: *mut std::ffi::c_void,
    drop: fn(*mut std::ffi::c_void),
}

#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub struct KcpConfig {
    pub nc: bool,
    pub mtu: u32,
    pub timeout: u32,
    pub nodelay: bool,
    pub interval: i32,
    pub resend: i32,
    pub rcvwnd_size: i32,
    pub sndwnd_size: i32,
    pub close_delay: u32,
}

pub const FAST_MODE: KcpConfig = KcpConfig {
    nc: true,
    mtu: 1400,
    timeout: 15000,
    nodelay: true,
    interval: 5,
    resend: 2,
    rcvwnd_size: 2048,
    sndwnd_size: 2048,
    close_delay: 10000,
};

pub const NORMAL_MODE: KcpConfig = KcpConfig {
    nc: false,
    mtu: 1400,
    timeout: 15000,
    nodelay: false,
    interval: 40,
    resend: 0,
    rcvwnd_size: 1024,
    sndwnd_size: 512,
    close_delay: 15000,
};

#[derive(Debug)]
pub struct Kcp<A: ConvAllocator> {
    conv: ikcp::CONV_T,
    ikcp: ikcp::CB,
    user: Option<User>,
    config: KcpConfig,
    allocator: A,
}

pub trait ConvAllocator: Send {
    fn allocate(&mut self) -> error::Result<ikcp::CONV_T>;
    fn deallocate(&mut self, conv: ikcp::CONV_T);
}

const IKCP_WND_RCV: usize = 128;

unsafe impl<A> Send for Kcp<A> where A: ConvAllocator {}
unsafe impl<A> Sync for Kcp<A> where A: ConvAllocator {}

impl<A> Kcp<A>
where
    A: ConvAllocator,
{
    pub fn new<U>(
        mut allocator: A,
        config: KcpConfig,
        user: Option<(*mut U, fn(*mut std::ffi::c_void))>,
    ) -> error::Result<Self> {
        let conv = allocator.allocate()?;

        let (user_data, user) = match user {
            None => (std::ptr::null_mut(), None),
            Some((user, drop)) => (
                user as *mut std::ffi::c_void,
                Some(User {
                    drop,
                    user: user as *mut std::ffi::c_void,
                }),
            ),
        };

        let ikcp = unsafe { ikcp::create(conv, user_data) };

        if ikcp.is_null() {
            return Err(KcpErrorKind::CreateFail.into());
        }

        unsafe {
            ikcp::setmtu(ikcp, config.mtu as i32);
            ikcp::wndsize(ikcp, config.sndwnd_size, config.rcvwnd_size);

            ikcp::nodelay(
                ikcp,
                config.nodelay,
                config.interval,
                config.resend,
                config.nc,
            );
        }

        Ok(Self {
            conv,
            ikcp,
            user,
            config,
            allocator,
        })
    }

    pub fn new_fast<U>(
        allocator: A,
        user: Option<(*mut U, fn(*mut std::ffi::c_void))>,
    ) -> error::Result<Self> {
        Self::new(allocator, FAST_MODE, user)
    }

    pub fn new_normal<U>(
        allocator: A,
        user: Option<(*mut U, fn(*mut std::ffi::c_void))>,
    ) -> error::Result<Self> {
        Self::new(allocator, NORMAL_MODE, user)
    }

    pub fn conv(&self) -> u32 {
        self.conv
    }

    pub fn get_conv(pkt: &[u8]) -> u32 {
        unsafe { ikcp::getconv(pkt.as_ptr() as *const std::ffi::c_void) }
    }

    pub fn recv(&self, buf: &mut [u8]) -> error::Result<usize> {
        let retval = unsafe { ikcp::recv(self.ikcp, buf.as_mut_ptr(), buf.len() as i32) };

        if retval == -3 {
            Err(error::KcpErrorKind::BufferTooSmall(buf.len(), self.peeksize() as usize).into())
        } else {
            Ok(retval as usize)
        }
    }

    pub fn send<D>(&self, pkt: D) -> error::Result<usize>
    where
        D: AsRef<[u8]>,
    {
        let mut data = pkt.as_ref();
        let mss = self.get_mss() as usize;

        if data.len() > mss {
            let count = (data.len() + mss - 1) / mss;
            if count >= IKCP_WND_RCV {
                let size = data.len() - (mss * (count - IKCP_WND_RCV)) - mss;
                data = &data[..size]
            }
        }

        let retval = unsafe { ikcp::send(self.ikcp, data.as_ptr(), data.len() as i32) };

        if retval < 0 {
            return Err(error::KcpError::WriteError(retval));
        } else {
            self.flush();

            Ok(retval as usize)
        }
    }

    pub fn flush(&self) {
        unsafe { ikcp::flush(self.ikcp) }
    }

    pub fn input<D>(&self, pkt: D) -> error::Result<()>
    where
        D: AsRef<[u8]>,
    {
        let data = pkt.as_ref();

        let retval = unsafe { ikcp::input(self.ikcp, data.as_ptr(), data.len()) };

        if retval < 0 {
            Err(KcpErrorKind::InputError(retval).into())
        } else {
            Ok(())
        }
    }

    pub fn get_mss(&self) -> i32 {
        unsafe { ikcp::get_mss(self.ikcp) }
    }

    pub fn set_minrto(&self, minrto: i32) {
        unsafe {
            ikcp::set_minrto(self.ikcp, minrto);
        }
    }

    pub fn update(&self, current: u32) {
        unsafe { ikcp::update(self.ikcp, current) }
    }

    pub fn check(&self, current: u32) -> u32 {
        unsafe { ikcp::check(self.ikcp, current) }
    }

    pub fn peeksize(&self) -> i32 {
        unsafe { ikcp::peeksize(self.ikcp) }
    }

    pub fn waitsnd(&self) -> u32 {
        unsafe { ikcp::waitsnd(self.ikcp) as u32 }
    }

    pub fn set_output(&self, output: ikcp::OutputFn) {
        unsafe { ikcp::setoutput(self.ikcp, output) }
    }

    pub fn sndwnd_size(&self) -> i32 {
        self.config.sndwnd_size
    }

    pub fn close_delay(&self) -> u32 {
        self.config.close_delay
    }

    pub fn timeout(&self) -> u32 {
        self.config.timeout
    }
}

impl<A> Drop for Kcp<A>
where
    A: ConvAllocator,
{
    fn drop(&mut self) {
        unsafe {
            ikcp::release(self.ikcp);
        }

        if let Some(user) = &self.user {
            let drop = user.drop;
            drop(user.user);
        }

        self.allocator.deallocate(self.conv);
    }
}
