//! A non-blocking eventfd. A thread blocked in `poll` wakes when another thread
//! pokes it, so a job does not wait for a timer and a timer is not required to
//! notice a job.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

pub struct Wake(OwnedFd);

impl Wake {
    pub fn pair() -> io::Result<(Wake, Wake)> {
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        let clone = owned.try_clone()?;
        Ok((Wake(owned), Wake(clone)))
    }

    pub fn poke(&self) {
        let one = 1u64.to_ne_bytes();
        unsafe {
            libc::write(self.0.as_raw_fd(), one.as_ptr().cast(), one.len());
        }
    }

    pub fn drain(&self) {
        let mut buf = [0u8; 8];
        loop {
            let n = unsafe { libc::read(self.0.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
        }
    }

    pub fn raw(&self) -> std::os::fd::RawFd {
        self.0.as_raw_fd()
    }
}
