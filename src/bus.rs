// SPDX-License-Identifier: GPL-3.0-or-later
//
// I2C access. `I2cBus` is the seam between the pure protocol code and the
// hardware; tests use a mock implementation.

use crate::protocol::{self, PROBE_PACKET, PROBE_REPLY_LEN, Packet, ProbeVerdict};
use log::debug;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

/// A raw I2C bus: plain write and plain read transfers, each a single message.
pub trait I2cBus {
    fn write(&mut self, addr: u16, data: &[u8]) -> io::Result<()>;
    fn read(&mut self, addr: u16, buf: &mut [u8]) -> io::Result<()>;
}

impl<T: I2cBus + ?Sized> I2cBus for Box<T> {
    fn write(&mut self, addr: u16, data: &[u8]) -> io::Result<()> {
        (**self).write(addr, data)
    }
    fn read(&mut self, addr: u16, buf: &mut [u8]) -> io::Result<()> {
        (**self).read(addr, buf)
    }
}

// From <linux/i2c-dev.h> and <linux/i2c.h>.
const I2C_RDWR: u64 = 0x0707;
const I2C_M_RD: u16 = 0x0001;

#[repr(C)]
struct I2cMsg {
    addr: u16,
    flags: u16,
    len: u16,
    buf: *mut u8,
}

#[repr(C)]
struct I2cRdwrIoctlData {
    msgs: *mut I2cMsg,
    nmsgs: u32,
}

/// A `/dev/i2c-N` character device, driven with the `I2C_RDWR` ioctl.
pub struct LinuxI2cBus {
    file: File,
    path: PathBuf,
}

impl LinuxI2cBus {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(LinuxI2cBus {
            file,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn transfer(&mut self, addr: u16, flags: u16, buf: &mut [u8]) -> io::Result<()> {
        let len = u16::try_from(buf.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "I2C message too long"))?;
        let mut msg = I2cMsg {
            addr,
            flags,
            len,
            buf: buf.as_mut_ptr(),
        };
        let mut data = I2cRdwrIoctlData {
            msgs: &mut msg,
            nmsgs: 1,
        };
        // SAFETY: `data` points at one valid `i2c_msg` whose buffer is `buf`, which
        // outlives the call and is exactly `len` bytes long. The kernel copies the
        // buffer in (write) or out (read) and keeps no pointer after returning.
        let ret = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                I2C_RDWR as _,
                &mut data as *mut I2cRdwrIoctlData,
            )
        };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl I2cBus for LinuxI2cBus {
    fn write(&mut self, addr: u16, data: &[u8]) -> io::Result<()> {
        debug!(
            "i2c write {} @0x{addr:02x}: {}",
            self.path.display(),
            protocol::hex(data)
        );
        let mut buf = data.to_vec();
        self.transfer(addr, 0, &mut buf)
    }

    fn read(&mut self, addr: u16, buf: &mut [u8]) -> io::Result<()> {
        self.transfer(addr, I2C_M_RD, buf)?;
        debug!(
            "i2c read  {} @0x{addr:02x}: {}",
            self.path.display(),
            protocol::hex(buf)
        );
        Ok(())
    }
}

#[derive(Debug)]
pub enum ProbeError {
    Io(io::Error),
    /// The device answered, but not like a Fusion 2 controller.
    Rejected {
        reply: [u8; PROBE_REPLY_LEN],
        verdict: ProbeVerdict,
    },
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProbeError::Io(e) => write!(f, "I2C transfer failed: {e}"),
            ProbeError::Rejected { reply, verdict } => {
                let why = match verdict {
                    ProbeVerdict::Echo => "bus echoes the write back (all 0xab)",
                    ProbeVerdict::WrongReply => "first byte is not 0xab",
                    ProbeVerdict::Valid => "valid",
                };
                write!(
                    f,
                    "not a Gigabyte RGB Fusion 2 GPU controller: reply {} ({why})",
                    protocol::hex(reply)
                )
            }
        }
    }
}

impl std::error::Error for ProbeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ProbeError::Io(e) => Some(e),
            ProbeError::Rejected { .. } => None,
        }
    }
}

/// Probes for the controller: write `AB 00..`, then read 4 bytes in a separate transfer.
/// Returns the reply on success.
pub fn probe<B: I2cBus + ?Sized>(
    bus: &mut B,
    addr: u16,
) -> Result<[u8; PROBE_REPLY_LEN], ProbeError> {
    bus.write(addr, &PROBE_PACKET).map_err(ProbeError::Io)?;
    let mut reply = [0u8; PROBE_REPLY_LEN];
    bus.read(addr, &mut reply).map_err(ProbeError::Io)?;
    match protocol::check_probe_reply(&reply) {
        ProbeVerdict::Valid => Ok(reply),
        verdict => Err(ProbeError::Rejected { reply, verdict }),
    }
}

/// Writes packets in order, stopping at the first error.
pub fn write_packets<B: I2cBus + ?Sized>(
    bus: &mut B,
    addr: u16,
    packets: &[Packet],
) -> io::Result<()> {
    for p in packets {
        bus.write(addr, p)?;
    }
    Ok(())
}

/// Test double that records every transfer.
#[doc(hidden)]
pub mod mock {
    use super::I2cBus;
    use std::collections::VecDeque;
    use std::io;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Op {
        Write(u16, Vec<u8>),
        Read(u16, usize),
    }

    #[derive(Default)]
    struct Inner {
        ops: Vec<Op>,
        replies: VecDeque<Vec<u8>>,
        fail_writes: bool,
    }

    /// Cloneable handle; all clones share the same log.
    #[derive(Clone, Default)]
    pub struct MockBus(Arc<Mutex<Inner>>);

    impl MockBus {
        pub fn new() -> Self {
            Self::default()
        }

        /// Queues the bytes returned by the next read.
        pub fn push_reply(&self, bytes: &[u8]) {
            self.0.lock().unwrap().replies.push_back(bytes.to_vec());
        }

        pub fn fail_writes(&self, fail: bool) {
            self.0.lock().unwrap().fail_writes = fail;
        }

        pub fn ops(&self) -> Vec<Op> {
            self.0.lock().unwrap().ops.clone()
        }

        pub fn writes(&self) -> Vec<Vec<u8>> {
            self.ops()
                .into_iter()
                .filter_map(|op| match op {
                    Op::Write(_, d) => Some(d),
                    Op::Read(..) => None,
                })
                .collect()
        }

        pub fn clear(&self) {
            self.0.lock().unwrap().ops.clear();
        }
    }

    impl I2cBus for MockBus {
        fn write(&mut self, addr: u16, data: &[u8]) -> io::Result<()> {
            let mut inner = self.0.lock().unwrap();
            if inner.fail_writes {
                return Err(io::Error::from_raw_os_error(libc::EIO));
            }
            inner.ops.push(Op::Write(addr, data.to_vec()));
            Ok(())
        }

        fn read(&mut self, addr: u16, buf: &mut [u8]) -> io::Result<()> {
            let mut inner = self.0.lock().unwrap();
            inner.ops.push(Op::Read(addr, buf.len()));
            let reply = inner
                .replies
                .pop_front()
                .ok_or_else(|| io::Error::from_raw_os_error(libc::ENXIO))?;
            let n = reply.len().min(buf.len());
            buf[..n].copy_from_slice(&reply[..n]);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::{MockBus, Op};
    use super::*;

    #[test]
    fn probe_accepts_version_reply() {
        let mut bus = MockBus::new();
        bus.push_reply(&[0xAB, 0x10, 0x52, 0x07]);
        assert_eq!(probe(&mut bus, 0x62).unwrap(), [0xAB, 0x10, 0x52, 0x07]);
        // A write then a separate 4-byte read, both at the card address.
        assert_eq!(
            bus.ops(),
            vec![
                Op::Write(0x62, vec![0xAB, 0, 0, 0, 0, 0, 0, 0]),
                Op::Read(0x62, 4)
            ]
        );
    }

    #[test]
    fn probe_rejects_echo() {
        let mut bus = MockBus::new();
        bus.push_reply(&[0xAB; 4]);
        match probe(&mut bus, 0x62) {
            Err(ProbeError::Rejected { reply, verdict }) => {
                assert_eq!(reply, [0xAB; 4]);
                assert_eq!(verdict, ProbeVerdict::Echo);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn probe_rejects_wrong_reply() {
        let mut bus = MockBus::new();
        bus.push_reply(&[0x00, 0x00, 0x00, 0x00]);
        assert!(matches!(
            probe(&mut bus, 0x62),
            Err(ProbeError::Rejected {
                verdict: ProbeVerdict::WrongReply,
                ..
            })
        ));
    }

    #[test]
    fn probe_reports_io_errors() {
        let mut bus = MockBus::new();
        // No reply queued: the read fails like a NAK.
        assert!(matches!(probe(&mut bus, 0x62), Err(ProbeError::Io(_))));
        let mut bus = MockBus::new();
        bus.fail_writes(true);
        assert!(matches!(probe(&mut bus, 0x62), Err(ProbeError::Io(_))));
        assert!(bus.ops().is_empty());
    }

    #[test]
    fn write_packets_in_order() {
        let mut bus = MockBus::new();
        let pk = [[1u8; 8], [2u8; 8], [3u8; 8]];
        write_packets(&mut bus, 0x62, &pk).unwrap();
        assert_eq!(
            bus.writes(),
            pk.iter().map(|p| p.to_vec()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn ioctl_structs_match_kernel_layout() {
        // struct i2c_msg { __u16 addr; __u16 flags; __u16 len; __u8 *buf; }
        assert_eq!(
            std::mem::size_of::<I2cMsg>(),
            8 + std::mem::size_of::<usize>()
        );
        assert_eq!(std::mem::offset_of!(I2cMsg, buf), 8);
        // struct i2c_rdwr_ioctl_data { struct i2c_msg *msgs; __u32 nmsgs; }
        assert_eq!(
            std::mem::offset_of!(I2cRdwrIoctlData, nmsgs),
            std::mem::size_of::<usize>()
        );
    }
}
