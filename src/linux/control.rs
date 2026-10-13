//! L2CAP attach for the accessory control service.
//! Opening the socket is not the handshake and does not mean the link is ready.

use std::io;
use std::os::fd::RawFd;

use crate::types::Address;

use super::bluez::{bdaddr, CONTROL_PSM};

const AF_BLUETOOTH: i32 = 31;
const BTPROTO_L2CAP: i32 = 0;
const SOL_BLUETOOTH: i32 = 274;
const BT_SECURITY: i32 = 4;
const BT_SECURITY_MEDIUM: u8 = 2;

#[repr(C)]
struct BtSecurity {
    level: u8,
    key_size: u8,
}

#[repr(C)]
struct SockAddrL2 {
    family: u16,
    psm: u16,
    bdaddr: [u8; 6],
    cid: u16,
    bdaddr_type: u8,
}

#[derive(Default)]
pub struct ControlLink {
    fd: Option<RawFd>,
    connecting: bool,
    serial: u64,
}

pub enum Attach {
    Up,
    Connecting,
}

impl ControlLink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_open(&self) -> bool {
        self.fd.is_some() && !self.connecting
    }

    pub fn fd(&self) -> Option<RawFd> {
        self.fd
    }

    pub fn identity(&self) -> Option<(RawFd, u64)> {
        self.fd.map(|fd| (fd, self.serial))
    }

    pub fn connecting(&self) -> bool {
        self.connecting
    }

    pub fn close(&mut self) {
        self.serial = self.serial.wrapping_add(1);
        if let Some(fd) = self.fd.take() {
            unsafe { libc::close(fd) };
        }
        self.connecting = false;
    }

    /// Bind any local address and start a non-blocking connect.
    pub fn attach(&mut self, address: Address) -> io::Result<Attach> {
        if self.fd.is_some() {
            return Ok(if self.connecting { Attach::Connecting } else { Attach::Up });
        }
        let fd = unsafe {
            libc::socket(AF_BLUETOOTH, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, BTPROTO_L2CAP)
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        if let Err(err) =
            set_nonblocking(fd).and_then(|_| require_encryption(fd)).and_then(|_| bind_any(fd))
        {
            unsafe { libc::close(fd) };
            return Err(err);
        }
        match connect_to(fd, address) {
            Ok(()) => {
                self.fd = Some(fd);
                Ok(Attach::Up)
            }
            Err(err)
                if err.raw_os_error() == Some(libc::EINPROGRESS)
                    || err.kind() == io::ErrorKind::WouldBlock =>
            {
                self.fd = Some(fd);
                self.connecting = true;
                Ok(Attach::Connecting)
            }
            Err(err) => {
                unsafe { libc::close(fd) };
                Err(err)
            }
        }
    }

    /// Complete a connect that returned [`Attach::Connecting`].
    pub fn finish_connect(&mut self) -> io::Result<bool> {
        let Some(fd) = self.fd else {
            return Ok(false);
        };
        if !self.connecting {
            return Ok(true);
        }
        let err = socket_error(fd)?;
        if err == 0 {
            self.connecting = false;
            Ok(true)
        } else if err == libc::EINPROGRESS || err == libc::EALREADY {
            Ok(false)
        } else {
            self.close();
            Err(io::Error::from_raw_os_error(err))
        }
    }

    pub fn send(&self, bytes: &[u8]) -> io::Result<()> {
        let Some(fd) = self.fd else {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "control socket is closed"));
        };
        let n = unsafe { libc::send(fd, bytes.as_ptr().cast(), bytes.len(), libc::MSG_NOSIGNAL) };
        if n == bytes.len() as isize {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// One read is one packet. An empty vec means nothing is waiting.
    pub fn read_packets(&mut self) -> io::Result<Vec<Vec<u8>>> {
        let Some(fd) = self.fd else {
            return Ok(Vec::new());
        };
        let mut packets = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n > 0 {
                packets.push(buf[..n as usize].to_vec());
                continue;
            }
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::Interrupted
                {
                    return Ok(packets);
                }
                self.close();
                return Err(err);
            }
            self.close();
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "control socket closed"));
        }
    }
}

impl Drop for ControlLink {
    fn drop(&mut self) {
        self.close();
    }
}

fn bind_any(fd: RawFd) -> io::Result<()> {
    let addr =
        SockAddrL2 { family: AF_BLUETOOTH as u16, psm: 0, bdaddr: [0; 6], cid: 0, bdaddr_type: 0 };
    let rc = unsafe {
        libc::bind(
            fd,
            std::ptr::from_ref(&addr).cast(),
            std::mem::size_of::<SockAddrL2>() as libc::socklen_t,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn connect_to(fd: RawFd, address: Address) -> io::Result<()> {
    let addr = SockAddrL2 {
        family: AF_BLUETOOTH as u16,
        psm: CONTROL_PSM,
        bdaddr: bdaddr(address),
        cid: 0,
        bdaddr_type: 0,
    };
    let rc = unsafe {
        libc::connect(
            fd,
            std::ptr::from_ref(&addr).cast(),
            std::mem::size_of::<SockAddrL2>() as libc::socklen_t,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

// AACP needs the bonded, encrypted link even when A2DP has not connected first.
fn require_encryption(fd: RawFd) -> io::Result<()> {
    let security = BtSecurity { level: BT_SECURITY_MEDIUM, key_size: 0 };
    let rc = unsafe {
        libc::setsockopt(
            fd,
            SOL_BLUETOOTH,
            BT_SECURITY,
            std::ptr::from_ref(&security).cast(),
            std::mem::size_of::<BtSecurity>() as libc::socklen_t,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn socket_error(fd: RawFd) -> io::Result<libc::c_int> {
    let mut err = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            std::ptr::from_mut(&mut err).cast(),
            &mut len,
        )
    };
    if rc == 0 {
        Ok(err)
    } else {
        Err(io::Error::last_os_error())
    }
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
