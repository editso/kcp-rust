pub mod ikcp {
    #![allow(unused)]

    use std::ffi::c_void;
    pub type CB = *mut ();

    pub type OutputFn =
        extern "C" fn(buf: *const u8, len: i32, kcp: CB, user: *mut c_void) -> i32;

    #[allow(non_camel_case_types)]
    pub type CONV_T = u32;

    #[link(name = "kcp", kind = "static")]
    extern "C" {

        #[link_name = "ikcp_create"]
        pub(crate) fn create(conv: CONV_T, user: *mut c_void) -> CB;

        #[link_name = "ikcp_release"]
        pub(crate) fn release(kcp: CB);

        #[link_name = "ikcp_setoutput"]
        pub(crate) fn setoutput(kcp: CB, output: OutputFn);

        #[link_name = "ikcp_recv"]
        pub(crate) fn recv(kcp: CB, buffer: *mut u8, len: i32) -> i32;

        #[link_name = "ikcp_send"]
        pub(crate) fn send(kcp: CB, buffer: *const u8, len: i32) -> i32;

        #[link_name = "ikcp_update"]
        pub(crate) fn update(kcp: CB, current: u32);

        #[link_name = "ikcp_check"]
        pub(crate) fn check(kcp: CB, current: u32) -> u32;

        #[link_name = "ikcp_input"]
        pub(crate) fn input(kcp: CB, data: *const u8, size: usize) -> i32;

        #[link_name = "ikcp_flush"]
        pub(crate) fn flush(kcp: CB);

        #[link_name = "ikcp_peeksize"]
        pub(crate) fn peeksize(kcp: CB) -> i32;

        #[link_name = "ikcp_setmtu"]
        pub(crate) fn setmtu(kcp: CB, mtu: i32) -> i32;

        #[link_name = "ikcp_wndsize"]
        pub(crate) fn wndsize(kcp: CB, sndwnd: i32, rcvwnd: i32) -> i32;

        #[link_name = "ikcp_waitsnd"]
        pub(crate) fn waitsnd(kcp: CB) -> i32;

        #[link_name = "ikcp_nodelay"]
        pub(crate) fn nodelay(kcp: CB, nodelay: i32, interval: i32, resend: i32, nc: i32) -> i32;

        #[link_name = "ikcp_getconv"]
        pub(crate) fn getconv(ptr: *const c_void) -> CONV_T;
    }
}
