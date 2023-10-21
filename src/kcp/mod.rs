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

#[derive(Debug)]
pub struct Kcp<A: ConvAllocator> {
    conv: ikcp::CONV_T,
    ikcp: ikcp::CB,
    user: Option<User>,
    allocator: A,
}

pub trait ConvAllocator: Send {
    fn allocate(&mut self) -> error::Result<ikcp::CONV_T>;
    fn deallocate(&mut self, conv: ikcp::CONV_T);
}

unsafe impl<A> Send for Kcp<A> where A: ConvAllocator {}
unsafe impl<A> Sync for Kcp<A> where A: ConvAllocator {}

impl<A> Kcp<A>
where
    A: ConvAllocator,
{
    pub fn new<U>(
        mut allocator: A,
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
            Err(KcpErrorKind::CreateFail.into())
        } else {
            Ok(Self {
                conv,
                ikcp,
                user,
                allocator,
            })
        }
    }

    pub fn get_conv(pkt: &[u8]) -> u32 {
        unsafe { ikcp::getconv(pkt.as_ptr() as *const std::ffi::c_void) }
    }

    pub fn recv(&self, buf: &mut [u8]) -> error::Result<usize> {
        let retval = unsafe { ikcp::recv(self.ikcp, buf.as_mut_ptr(), buf.len() as i32) };
        Ok(retval as usize)
    }

    pub fn send<D>(&self, pkt: D) -> error::Result<usize>
    where
        D: AsRef<[u8]>,
    {
        let data = pkt.as_ref();

        let retval = unsafe { ikcp::send(self.ikcp, data.as_ptr(), data.len() as i32) };

        Ok(retval as usize)
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
        Ok(())
    }

    pub fn update(&self, current: u32) {
        unsafe { ikcp::update(self.ikcp, current) }
    }

    pub fn check(&self, current: u32) -> u32 {
        unsafe { ikcp::check(self.ikcp, current) }
    }

    pub fn waitsnd(&self) -> u32 {
        unsafe { ikcp::waitsnd(self.ikcp) as u32}
    }

    pub fn set_output(&self, output: ikcp::OutputFn) {
        unsafe { ikcp::setoutput(self.ikcp, output) }
    }
}

impl<A> Drop for Kcp<A>
where
    A: ConvAllocator,
{
    fn drop(&mut self) {
        self.allocator.deallocate(self.conv);

        unsafe {
            ikcp::release(self.ikcp);
        }

        if let Some(user) = &self.user {
            let drop = user.drop;
            drop(user.user);
        }
    }
}
