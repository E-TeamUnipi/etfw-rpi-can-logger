//! Raw SocketCAN socket bound to *all* CAN interfaces (ifindex 0), so
//! adapters plugged in later are picked up without reopening anything.

use std::io;
use std::mem::{size_of, zeroed};
use std::os::unix::io::RawFd;

const PF_CAN: i32 = 29;
const CAN_RAW: i32 = 1;
const SOL_CAN_RAW: i32 = 101;
const CAN_RAW_ERR_FILTER: i32 = 2;
const CAN_RAW_FD_FRAMES: i32 = 5;
const CAN_ERR_MASK: u32 = 0x1FFF_FFFF;
const CAN_MTU: usize = 16;
const CANFD_MTU: usize = 72;

#[repr(C, align(8))]
struct SockaddrCan {
    family: u16,
    ifindex: i32,
    addr: [u8; 16],
}

pub struct RxFrame {
    pub ifindex: i32,
    /// Kernel receive timestamp (CLOCK_REALTIME), if available.
    pub ts_rt_ns: Option<i64>,
    pub id: u32,
    pub fd: bool,
    pub flags: u8,
    pub len: u8,
    pub data: [u8; 64],
}

const BATCH: usize = 64;

pub struct CanSocket {
    pub fd: RawFd,
    bufs: Box<[[u8; CANFD_MTU]; BATCH]>,
    addrs: Box<[SockaddrCan; BATCH]>,
    ctrl: Box<[[u64; 16]; BATCH]>,
    iov: Box<[libc::iovec; BATCH]>,
    msgs: Box<[libc::mmsghdr; BATCH]>,
    last_ovfl: Option<u32>,
}

unsafe impl Send for CanSocket {}

fn setopt<T>(fd: RawFd, level: i32, name: i32, v: &T) -> io::Result<()> {
    let r = unsafe { libc::setsockopt(fd, level, name, v as *const T as *const libc::c_void, size_of::<T>() as u32) };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

impl CanSocket {
    pub fn open(rcvbuf: usize) -> io::Result<CanSocket> {
        let fd = unsafe { libc::socket(PF_CAN, libc::SOCK_RAW | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, CAN_RAW) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let one: i32 = 1;
        setopt(fd, SOL_CAN_RAW, CAN_RAW_FD_FRAMES, &one)?;
        setopt(fd, SOL_CAN_RAW, CAN_RAW_ERR_FILTER, &CAN_ERR_MASK)?;
        setopt(fd, libc::SOL_SOCKET, libc::SO_TIMESTAMPNS, &one)?;
        let _ = setopt(fd, libc::SOL_SOCKET, libc::SO_RXQ_OVFL, &one);
        let size = rcvbuf as i32;
        if setopt(fd, libc::SOL_SOCKET, libc::SO_RCVBUFFORCE, &size).is_err() {
            let _ = setopt(fd, libc::SOL_SOCKET, libc::SO_RCVBUF, &size);
        }
        let addr = SockaddrCan { family: PF_CAN as u16, ifindex: 0, addr: [0; 16] };
        let r = unsafe { libc::bind(fd, &addr as *const _ as *const libc::sockaddr, size_of::<SockaddrCan>() as u32) };
        if r < 0 {
            let e = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(e);
        }
        Ok(CanSocket {
            fd,
            bufs: Box::new([[0; CANFD_MTU]; BATCH]),
            addrs: Box::new(unsafe { zeroed() }),
            ctrl: Box::new([[0; 16]; BATCH]),
            iov: Box::new(unsafe { zeroed() }),
            msgs: Box::new(unsafe { zeroed() }),
            last_ovfl: None,
        })
    }

    /// Receive up to 64 frames without blocking. Returns the frames and how
    /// many frames the kernel dropped since the previous call (socket queue
    /// overflow).
    pub fn recv_batch(&mut self, out: &mut Vec<RxFrame>) -> io::Result<u32> {
        for i in 0..BATCH {
            self.iov[i] = libc::iovec { iov_base: self.bufs[i].as_mut_ptr() as *mut libc::c_void, iov_len: CANFD_MTU };
            let h = &mut self.msgs[i].msg_hdr;
            h.msg_name = &mut self.addrs[i] as *mut _ as *mut libc::c_void;
            h.msg_namelen = size_of::<SockaddrCan>() as u32;
            h.msg_iov = &mut self.iov[i];
            h.msg_iovlen = 1;
            h.msg_control = self.ctrl[i].as_mut_ptr() as *mut libc::c_void;
            h.msg_controllen = size_of::<[u64; 16]>() as _;
            h.msg_flags = 0;
            self.msgs[i].msg_len = 0;
        }
        let n = unsafe { libc::recvmmsg(self.fd, self.msgs.as_mut_ptr(), BATCH as u32, libc::MSG_DONTWAIT, std::ptr::null_mut()) };
        if n < 0 {
            let e = io::Error::last_os_error();
            return match e.raw_os_error() {
                Some(libc::EAGAIN) | Some(libc::EINTR) => Ok(0),
                // ENETDOWN happens when an interface goes away; not fatal
                Some(libc::ENETDOWN) | Some(libc::ENXIO) | Some(libc::ENODEV) => Ok(0),
                _ => Err(e),
            };
        }
        let mut dropped = 0u32;
        for i in 0..n as usize {
            let len = self.msgs[i].msg_len as usize;
            if len != CAN_MTU && len != CANFD_MTU {
                continue; // CAN XL or something unexpected
            }
            let b = &self.bufs[i];
            let mut f = RxFrame {
                ifindex: self.addrs[i].ifindex,
                ts_rt_ns: None,
                id: u32::from_ne_bytes([b[0], b[1], b[2], b[3]]),
                fd: len == CANFD_MTU,
                flags: 0,
                len: b[4],
                data: [0; 64],
            };
            let max = if f.fd { 64 } else { 8 };
            if f.fd {
                f.flags = b[5];
            }
            let dl = if !f.fd && f.id & canlog_core::format::CAN_RTR_FLAG != 0 { 0 } else { (f.len as usize).min(max) };
            f.data[..dl].copy_from_slice(&b[8..8 + dl]);
            if !f.fd {
                f.len = f.len.min(8);
            }
            // control messages: timestamp and drop counter
            unsafe {
                let h = &self.msgs[i].msg_hdr;
                let mut c = libc::CMSG_FIRSTHDR(h);
                while !c.is_null() {
                    let cm = &*c;
                    if cm.cmsg_level == libc::SOL_SOCKET && cm.cmsg_type == libc::SO_TIMESTAMPNS {
                        let ts = std::ptr::read_unaligned(libc::CMSG_DATA(c) as *const libc::timespec);
                        f.ts_rt_ns = Some(ts.tv_sec as i64 * 1_000_000_000 + ts.tv_nsec as i64);
                    } else if cm.cmsg_level == libc::SOL_SOCKET && cm.cmsg_type == libc::SO_RXQ_OVFL {
                        let v = std::ptr::read_unaligned(libc::CMSG_DATA(c) as *const u32);
                        if let Some(prev) = self.last_ovfl {
                            dropped = dropped.max(v.wrapping_sub(prev));
                        }
                        self.last_ovfl = Some(v);
                    }
                    c = libc::CMSG_NXTHDR(h, c);
                }
            }
            out.push(f);
        }
        Ok(dropped)
    }
}

impl Drop for CanSocket {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}
