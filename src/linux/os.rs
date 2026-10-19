//! The small set of Linux calls the runtime cannot avoid.
//! Policy does not live here.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

static STOP: AtomicBool = AtomicBool::new(false);

pub fn stop_requested() -> bool {
    STOP.load(Ordering::Relaxed)
}

pub fn request_stop() {
    STOP.store(true, Ordering::Relaxed);
}

extern "C" fn on_signal(_sig: i32) {
    STOP.store(true, Ordering::Relaxed);
}

pub fn install_stop_signals() {
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
    }
}

pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return "localhost".to_string();
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).trim().to_string()
}

pub fn random_bytes(buf: &mut [u8]) -> io::Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        let rc =
            unsafe { libc::getrandom(buf[filled..].as_mut_ptr().cast(), buf.len() - filled, 0) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        filled += rc as usize;
    }
    Ok(())
}

/// Wait for `fd` or `wake`. Reports whether `fd` is readable.
pub fn poll_in_or_wake(fd: i32, wake: Option<i32>, timeout_ms: i32) -> io::Result<bool> {
    let mut fds = [
        libc::pollfd { fd, events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: wake.unwrap_or(-1), events: libc::POLLIN, revents: 0 },
    ];
    let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
    if rc < 0 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(err);
    }
    Ok(fds[0].revents != 0)
}
