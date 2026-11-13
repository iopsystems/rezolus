//! Corsair AXi power supplies, read over the Corsair Link USB dongle.
//!
//! The dongle is a Silicon Labs CP210x USB-serial bridge (`1b1c:1c02`, bound to
//! `cp210x` as a `ttyUSB` device) in front of the PSU's PMBus. Corsair does not
//! document the protocol. What this module implements was taken from the
//! community `cpsumon` project as a description only and checked against
//! captures of an AX1500i (`fixtures/corsair-axi-ax1500i-*.jsonl`):
//!
//! - The port runs at 115200 baud, 8N1, raw.
//! - A frame is a start byte, two code bytes per payload byte (low nibble
//!   first, each nibble mapped through [`CODES`]) and a `0x00` terminator.
//!   Code bytes are `0x55..=0xaa`, so `0x00` occurs only as the terminator.
//!   Requests start with `0x54`; every reply captured starts with `0xa8`.
//! - Replies vary in length (the dongle's name is a 44-byte payload, 90 bytes
//!   framed), so a frame is read up to its terminator, never by count.
//! - A PMBus register read is two exchanges: `[19, 3, 6, 1, 7, len, reg]`,
//!   answered with an empty acknowledgement, then `[8, 7, len]`, answered with
//!   `len` data bytes. Readings are PMBus LINEAR11.
//!
//! Published registers: `READ_PIN` (input power), `READ_VIN`, `READ_IIN`,
//! `READ_TEMPERATURE_1`/`_2` and `READ_FAN_SPEED_1`. `READ_VIN` × `READ_IIN`
//! agrees with `READ_PIN` within 3.1% in the captures. Corsair's register
//! `0xee`, which `cpsumon` reports as output power, read 0 on most polls and
//! 346–348 W on a few, so it is not published. The fan read 0 rpm in every
//! capture, up to 371 W input.
//!
//! The dongle is sent its setup command (`[17, 2, 100, 0, 0, 0, 0]`) and PMBus
//! read requests; no PMBus register is written. What the setup command does is
//! undocumented. The PSU's PMBus page is read when a session opens and again
//! before a reading at most once a second; a page other than 0 fails the port
//! as a failed open does, and the page is not switched back.
//!
//! The port is refused if another process has set `TIOCEXCL` on it or holds a
//! `flock` on it, and is otherwise locked with `flock` before its settings are
//! changed, so a second agent or another tool that locks the port cannot
//! interleave with this one. The agent does not set `TIOCEXCL`: the kernel
//! keeps it after a close while another process has the port open, including
//! the close at exit. A port is closed on a separate thread, since
//! usb-serial's close can wait up to 30 s for output to drain. A process
//! that already has the port open without either (CyberPower's `pwrstatd`
//! probes serial ports for a UPS) still consumes replies; reads then time out
//! or fail to decode. Before each open
//! the tty is checked to still belong to a device with one of [`USB_IDS`],
//! since `ttyUSBn` numbers are reassigned when USB devices come and go.
//!
//! Failures back off. A failed open is not retried by a reading for
//! [`OPEN_BACKOFF`] (discovery retries on its own interval). A failed reading
//! drops the session, since the stream position is then unknown, and that
//! register is not read again for one second, doubling on each consecutive
//! failure up to [`MAX_REGISTER_BACKOFF`].

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// USB vendor and product IDs of the dongle. The dongle's product
/// string is generic, so the model published is the one the PSU reports.
pub(crate) const USB_IDS: &[(&str, &str)] = &[("1b1c", "1c02")];

pub(crate) const READ_VIN: u8 = 0x88;
pub(crate) const READ_IIN: u8 = 0x89;
pub(crate) const READ_TEMPERATURE_1: u8 = 0x8d;
pub(crate) const READ_TEMPERATURE_2: u8 = 0x8e;
pub(crate) const READ_FAN_SPEED_1: u8 = 0x90;
pub(crate) const READ_PIN: u8 = 0x97;
const PAGE: u8 = 0x00;
const MFR_MODEL: u8 = 0x9a;

/// Nibble value -> code byte.
const CODES: [u8; 16] = [
    0x55, 0x56, 0x59, 0x5a, 0x65, 0x66, 0x69, 0x6a, 0x95, 0x96, 0x99, 0x9a, 0xa5, 0xa6, 0xa9, 0xaa,
];
const REQUEST_START: u8 = 0x54;
const REPLY_START: u8 = 0xa8;
const TERMINATOR: u8 = 0x00;
/// A frame longer than this is not a frame this protocol produces.
const MAX_FRAME: usize = 1024;
/// How long a write, or a reply after it, may take before it is treated as
/// lost. The slowest reply in the captures arrived 31 ms after its request.
const REPLY_TIMEOUT: Duration = Duration::from_millis(250);

fn nibble(code: u8) -> Option<u8> {
    CODES.iter().position(|&c| c == code).map(|n| n as u8)
}

/// The frame for a request payload.
pub(crate) fn encode(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(2 * payload.len() + 2);
    frame.push(REQUEST_START);
    for &b in payload {
        frame.push(CODES[(b & 0xf) as usize]);
        frame.push(CODES[(b >> 4) as usize]);
    }
    frame.push(TERMINATOR);
    frame
}

/// The payload of a reply frame, which must include its terminator.
pub(crate) fn decode(frame: &[u8]) -> io::Result<Vec<u8>> {
    let invalid =
        |why: &str| io::Error::new(io::ErrorKind::InvalidData, format!("corsair_axi: {why}"));
    let (&start, rest) = frame.split_first().ok_or_else(|| invalid("empty frame"))?;
    if start != REPLY_START {
        return Err(invalid("reply does not start with 0xa8"));
    }
    let (&end, body) = rest
        .split_last()
        .ok_or_else(|| invalid("frame has no terminator"))?;
    if end != TERMINATOR || body.len() % 2 != 0 {
        return Err(invalid("frame is not code-byte pairs and a terminator"));
    }
    let (pairs, _) = body.as_chunks::<2>();
    pairs
        .iter()
        .map(|&[lo, hi]| match (nibble(lo), nibble(hi)) {
            (Some(lo), Some(hi)) => Ok(lo | hi << 4),
            _ => Err(invalid("byte outside the code table")),
        })
        .collect()
}

fn page_is_zero(page: &[u8]) -> io::Result<()> {
    if page != [0] {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("corsair_axi: PMBus page is {page:?}, not 0; not switching it"),
        ));
    }
    Ok(())
}

/// A PMBus LINEAR11 value: a 5-bit signed exponent over an 11-bit signed mantissa.
pub(crate) fn linear11(lo: u8, hi: u8) -> f64 {
    let raw = u16::from(lo) | u16::from(hi) << 8;
    let exponent = ((raw >> 11) as i8) << 3 >> 3;
    let mantissa = ((raw & 0x7ff) as i16) << 5 >> 5;
    f64::from(mantissa) * 2f64.powi(i32::from(exponent))
}

/// A byte stream with a read deadline: a serial port, or a replay in tests.
pub(crate) trait Port {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// One byte, or `TimedOut` once `deadline` passes.
    fn read_byte(&mut self, deadline: Instant) -> io::Result<u8>;
    /// Discard anything already received.
    fn discard_input(&mut self) -> io::Result<()>;
}

/// An open, handshaken session with one PSU.
pub(crate) struct Session<P> {
    port: P,
    model: String,
    page_checked: Instant,
}

impl<P: Port> Session<P> {
    /// Handshake with the dongle, read the PSU's model and check its PMBus page.
    pub(crate) fn open(mut port: P) -> io::Result<Self> {
        port.discard_input()?;
        let mut session = Self {
            port,
            model: String::new(),
            page_checked: Instant::now(),
        };
        session.exchange(&[2])?; // dongle name
        session.exchange(&[17, 2, 100, 0, 0, 0, 0])?; // dongle setup
        session.exchange(&[0])?; // dongle version
        let model = session.read_register(MFR_MODEL, 7)?;
        session.model = String::from_utf8_lossy(&model)
            .trim_end_matches(['\0', ' '])
            .to_string();
        if session.model.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "corsair_axi: empty model",
            ));
        }
        session.check_page()?;
        Ok(session)
    }

    /// Fails unless the PSU's PMBus page is 0. Another tool can leave it on
    /// another page between readings; this session never switches it.
    fn check_page(&mut self) -> io::Result<()> {
        let page = self.read_register(PAGE, 1)?;
        page_is_zero(&page)
    }

    pub(crate) fn model(&self) -> &str {
        &self.model
    }

    fn exchange(&mut self, payload: &[u8]) -> io::Result<Vec<u8>> {
        self.port.write_all(&encode(payload)).map_err(|e| {
            if e.kind() == io::ErrorKind::TimedOut {
                io::Error::new(io::ErrorKind::TimedOut, "corsair_axi: write timed out")
            } else {
                e
            }
        })?;
        let deadline = Instant::now() + REPLY_TIMEOUT;
        let mut frame = Vec::new();
        loop {
            let b = match self.port.read_byte(deadline) {
                Ok(b) => b,
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                    let why = if frame.is_empty() {
                        "corsair_axi: no reply (another process may have the port open, e.g. a UPS daemon)".to_string()
                    } else {
                        format!(
                            "corsair_axi: incomplete reply ({} bytes, no terminator)",
                            frame.len()
                        )
                    };
                    return Err(io::Error::new(io::ErrorKind::TimedOut, why));
                }
                Err(e) => return Err(e),
            };
            frame.push(b);
            if b == TERMINATOR {
                return decode(&frame);
            }
            if frame.len() > MAX_FRAME {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "corsair_axi: frame too long",
                ));
            }
        }
    }

    /// `len` bytes of a PMBus register.
    pub(crate) fn read_register(&mut self, register: u8, len: u8) -> io::Result<Vec<u8>> {
        let ack = self.exchange(&[19, 3, 6, 1, 7, len, register])?;
        if !ack.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "corsair_axi: unexpected acknowledgement",
            ));
        }
        let data = self.exchange(&[8, 7, len])?;
        if data.len() != usize::from(len) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "corsair_axi: register {register:#04x}: {} bytes, expected {len}",
                    data.len()
                ),
            ));
        }
        Ok(data)
    }

    /// A LINEAR11 register, in its PMBus unit (W, V, A, °C or rpm).
    pub(crate) fn read_linear11(&mut self, register: u8) -> io::Result<f64> {
        let data = self.read_register(register, 2)?;
        Ok(linear11(data[0], data[1]))
    }
}

/// A raw 115200 8N1 serial port.
pub(crate) struct SerialPort {
    /// Taken by [`Drop`], which closes it on another thread.
    fd: Option<OwnedFd>,
    tty: PathBuf,
}

/// Ports whose close is still running on a `corsair-axi-close` thread.
static CLOSING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn closing() -> std::sync::MutexGuard<'static, Vec<PathBuf>> {
    CLOSING.lock().unwrap_or_else(|e| e.into_inner())
}

/// How long an open waits for the port's previous close to return. A cp210x
/// close with no output to drain took 7.9 ms on the tested dongle.
const CLOSE_WAIT: Duration = Duration::from_millis(100);

/// The error for a port whose previous close has not returned. It is not a
/// failed open, so no [`OPEN_BACKOFF`] is set; the port is retried by the
/// first reading at least [`CLOSE_PENDING_RETRY`] later.
#[derive(Debug)]
pub(crate) struct ClosePending;

impl std::fmt::Display for ClosePending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("corsair_axi: the port's last close has not finished")
    }
}

impl std::error::Error for ClosePending {}

fn is_close_pending(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<ClosePending>())
}

impl SerialPort {
    fn raw(&self) -> i32 {
        self.fd.as_ref().expect("open until dropped").as_raw_fd()
    }

    fn open_fd(path: &Path) -> io::Result<OwnedFd> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        // SAFETY: c is a valid NUL-terminated path.
        let fd = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd was just returned by open and is owned here.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Sets the port to raw 115200 8N1 with non-blocking reads.
    fn configure_raw(&self) -> io::Result<()> {
        let raw = self.raw();
        // SAFETY: termios is plain data; tcgetattr fills it.
        let mut tio: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(raw, &mut tio) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: tio was initialised by tcgetattr.
        unsafe {
            libc::cfmakeraw(&mut tio);
            libc::cfsetispeed(&mut tio, libc::B115200);
            libc::cfsetospeed(&mut tio, libc::B115200);
        }
        tio.c_cflag |= libc::CLOCAL | libc::CREAD;
        tio.c_cc[libc::VMIN] = 0;
        tio.c_cc[libc::VTIME] = 0;
        if unsafe { libc::tcsetattr(raw, libc::TCSANOW, &tio) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Opens and configures `path` without claiming it.
    #[cfg(test)]
    fn open(path: &Path) -> io::Result<Self> {
        let port = Self {
            fd: Some(Self::open_fd(path)?),
            tty: path.to_path_buf(),
        };
        port.configure_raw()?;
        Ok(port)
    }

    /// Opens `path` for this agent's use, then configures it; see
    /// [`Self::claim`]. If the port's previous close is still running, waits
    /// up to [`CLOSE_WAIT`] for it, then refuses with [`ClosePending`]
    /// without opening the port, since the open would wait for that close.
    pub(crate) fn open_exclusive(path: &Path) -> io::Result<Self> {
        let started = Instant::now();
        while closing().iter().any(|p| p == path) {
            if started.elapsed() >= CLOSE_WAIT {
                return Err(io::Error::new(io::ErrorKind::ResourceBusy, ClosePending));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Self::claim(Self::open_fd(path)?, path)
    }

    /// Claims an open port, then configures it. The port is refused, before
    /// its termios is changed or anything is written, if another process has
    /// set `TIOCEXCL` on it (root can open such a port, so this is checked)
    /// or holds a `flock` on it. Once claimed, the `flock` refuses other
    /// lockers until the port closes.
    fn claim(fd: OwnedFd, tty: &Path) -> io::Result<Self> {
        let raw = fd.as_raw_fd();
        let mut excl: libc::c_int = 0;
        // SAFETY: raw is open; TIOCGEXCL writes one c_int.
        if unsafe { libc::ioctl(raw, libc::TIOCGEXCL, &mut excl) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if excl != 0 {
            return Err(io::Error::new(
                io::ErrorKind::ResourceBusy,
                "corsair_axi: another process has the port open exclusively",
            ));
        }
        // SAFETY: raw is open; flock takes no pointers.
        if unsafe { libc::flock(raw, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let err = io::Error::last_os_error();
            return Err(io::Error::new(
                err.kind(),
                format!("corsair_axi: port is locked by another process: {err}"),
            ));
        }
        let port = Self {
            fd: Some(fd),
            tty: tty.to_path_buf(),
        };
        port.configure_raw()?;
        Ok(port)
    }
}

impl Drop for SerialPort {
    /// Closes the port on a detached thread. usb-serial's final close waits up
    /// to `closing_wait` (30 s by default) for unsent output to reach a dongle
    /// that has stopped accepting it, and holds the tty's lock meanwhile, so a
    /// reopen would wait too; [`Self::open_exclusive`] refuses the port until
    /// the close returns. If the thread cannot be started, the port is closed
    /// here, on the caller's thread.
    fn drop(&mut self) {
        let Some(fd) = self.fd.take() else {
            return;
        };
        let tty = self.tty.clone();
        closing().push(tty.clone());
        let done = tty.clone();
        let spawned = std::thread::Builder::new()
            .name("corsair-axi-close".into())
            .spawn(move || {
                drop(fd);
                closing().retain(|p| *p != done);
            });
        if spawned.is_err() {
            // The closure, and the fd with it, was dropped by spawn.
            closing().retain(|p| *p != tty);
        }
    }
}

/// Whether the tty named by `tty` (`/dev/ttyUSBn`) is still a port of a USB
/// device with one of [`USB_IDS`]. The tty's `device` link is the usb-serial
/// port (`.../1-13/1-13:1.0/ttyUSB0`); the USB device is the nearest ancestor
/// with an `idVendor`.
pub(crate) fn is_dongle(sysfs: &Path, tty: &Path) -> bool {
    let Some(name) = tty.file_name() else {
        return false;
    };
    let Ok(port) = std::fs::canonicalize(sysfs.join("class/tty").join(name).join("device")) else {
        return false;
    };
    let id = |dir: &Path, file: &str| {
        std::fs::read_to_string(dir.join(file))
            .map(|v| v.trim().to_string())
            .ok()
    };
    let Some(device) = port
        .ancestors()
        .skip(1)
        .take(3)
        .find(|dir| dir.join("idVendor").exists())
    else {
        return false;
    };
    match (id(device, "idVendor"), id(device, "idProduct")) {
        (Some(vendor), Some(product)) => USB_IDS.iter().any(|(v, p)| *v == vendor && *p == product),
        _ => false,
    }
}

impl Port for SerialPort {
    fn write_all(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        let deadline = Instant::now() + REPLY_TIMEOUT;
        while !bytes.is_empty() {
            // SAFETY: bytes is a valid slice for its length.
            let n = unsafe { libc::write(self.raw(), bytes.as_ptr().cast(), bytes.len()) };
            if n > 0 {
                bytes = &bytes[n as usize..];
                continue;
            }
            let err = io::Error::last_os_error();
            match err.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => {}
                _ => return Err(err),
            }
            wait(self.raw(), libc::POLLOUT, deadline)?;
        }
        Ok(())
    }

    fn read_byte(&mut self, deadline: Instant) -> io::Result<u8> {
        loop {
            let mut b = 0u8;
            // SAFETY: b is one writable byte.
            let n = unsafe { libc::read(self.raw(), (&mut b as *mut u8).cast(), 1) };
            if n == 1 {
                return Ok(b);
            }
            if n == 0 {
                // In raw mode with VMIN 0 an empty tty reads 0; a hangup is
                // reported by poll.
                wait(self.raw(), libc::POLLIN, deadline)?;
                continue;
            }
            let err = io::Error::last_os_error();
            match err.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => {}
                _ => return Err(err),
            }
            wait(self.raw(), libc::POLLIN, deadline)?;
        }
    }

    fn discard_input(&mut self) -> io::Result<()> {
        // SAFETY: the fd is open.
        if unsafe { libc::tcflush(self.raw(), libc::TCIFLUSH) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Waits for `events` on `fd` until `deadline`. Returns `TimedOut` once the
/// deadline has passed, and an error if the port hangs up.
fn wait(fd: i32, events: libc::c_short, deadline: Instant) -> io::Result<()> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "corsair_axi: timed out",
        ));
    }
    let mut pfd = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
    // SAFETY: pfd is a valid pollfd for one descriptor.
    if unsafe { libc::poll(&mut pfd, 1, ms) } < 0 {
        let err = io::Error::last_os_error();
        return if err.kind() == io::ErrorKind::Interrupted {
            Ok(())
        } else {
            Err(err)
        };
    }
    if pfd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
        return Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "corsair_axi: port hung up",
        ));
    }
    Ok(())
}

/// After a failed open, a reading does not try the port again for this long.
pub(crate) const OPEN_BACKOFF: Duration = Duration::from_secs(60);
/// After a register's first failed reading, it is not read for this long;
/// each consecutive failure doubles the wait.
const REGISTER_BACKOFF: Duration = Duration::from_secs(1);
pub(crate) const MAX_REGISTER_BACKOFF: Duration = Duration::from_secs(60);
/// After an open finds the port's previous close still running, further opens
/// are refused without waiting for this long.
const CLOSE_PENDING_RETRY: Duration = Duration::from_secs(1);
/// The PMBus page is read again before a reading at most this often.
const PAGE_RECHECK: Duration = Duration::from_secs(1);

/// Opens a port, given the sysfs root and the tty.
type Opener<P> = Box<dyn Fn(&Path, &Path) -> io::Result<P> + Send>;

/// When something last failed, and how many times in a row.
#[derive(Clone, Copy)]
struct Failure {
    at: Instant,
    count: u32,
}

impl Failure {
    fn register_wait(&self) -> Duration {
        REGISTER_BACKOFF
            .saturating_mul(1 << self.count.saturating_sub(1).min(16))
            .min(MAX_REGISTER_BACKOFF)
    }
}

/// Open sessions by port, with the failures that hold back reopening a port
/// or rereading a register.
pub(crate) struct Sessions<P> {
    open: Opener<P>,
    sessions: HashMap<PathBuf, Session<P>>,
    open_failed: HashMap<PathBuf, Instant>,
    /// When an open last found the port's previous close still running.
    close_pending: HashMap<PathBuf, Instant>,
    register_failed: HashMap<(PathBuf, u8), Failure>,
}

impl<P: Port> Sessions<P> {
    pub(crate) fn new(open: impl Fn(&Path, &Path) -> io::Result<P> + Send + 'static) -> Self {
        Self {
            open: Box::new(open),
            sessions: HashMap::new(),
            open_failed: HashMap::new(),
            close_pending: HashMap::new(),
            register_failed: HashMap::new(),
        }
    }

    fn open(&mut self, sysfs: &Path, tty: &Path, now: Instant) -> io::Result<&mut Session<P>> {
        if !self.sessions.contains_key(tty) {
            // Within a second of finding the previous close still running, do
            // not wait for it again: one wait per second, not one per register.
            if let Some(at) = self.close_pending.get(tty) {
                if now.saturating_duration_since(*at) < CLOSE_PENDING_RETRY {
                    return Err(io::Error::new(io::ErrorKind::ResourceBusy, ClosePending));
                }
            }
            match (self.open)(sysfs, tty).and_then(Session::open) {
                Ok(mut session) => {
                    session.page_checked = now;
                    self.open_failed.remove(tty);
                    self.close_pending.remove(tty);
                    self.sessions.insert(tty.to_path_buf(), session);
                }
                Err(e) => {
                    if is_close_pending(&e) {
                        self.close_pending.insert(tty.to_path_buf(), now);
                    } else {
                        self.close_pending.remove(tty);
                        self.open_failed.insert(tty.to_path_buf(), now);
                    }
                    return Err(e);
                }
            }
        }
        Ok(self.sessions.get_mut(tty).expect("inserted above"))
    }

    /// Forgets every port not in `ttys`: its session (closing the port) and
    /// its failures. Discovery calls this with the ports it found, so a
    /// dongle that was unplugged does not keep an open session whose cached
    /// model a later device at the same `ttyUSBn` would inherit.
    pub(crate) fn retain(&mut self, ttys: &[PathBuf]) {
        self.sessions.retain(|tty, _| ttys.contains(tty));
        self.open_failed.retain(|tty, _| ttys.contains(tty));
        self.close_pending.retain(|tty, _| ttys.contains(tty));
        self.register_failed
            .retain(|(tty, _), _| ttys.contains(tty));
    }

    /// The model of the PSU on `tty`. Opens a session if none is open,
    /// whatever the open backoff: discovery runs on its own interval.
    pub(crate) fn model(&mut self, sysfs: &Path, tty: &Path, now: Instant) -> io::Result<String> {
        Ok(self.open(sysfs, tty, now)?.model().to_string())
    }

    /// A LINEAR11 register of the PSU on `tty`, in its PMBus unit.
    pub(crate) fn read(
        &mut self,
        sysfs: &Path,
        tty: &Path,
        register: u8,
        now: Instant,
    ) -> io::Result<f64> {
        let key = (tty.to_path_buf(), register);
        if let Some(failure) = self.register_failed.get(&key) {
            if now.saturating_duration_since(failure.at) < failure.register_wait() {
                return Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    format!(
                        "corsair_axi: register {register:#04x} failed {} time(s) in a row; waiting before retrying",
                        failure.count
                    ),
                ));
            }
        }
        if !self.sessions.contains_key(tty) {
            if let Some(at) = self.open_failed.get(tty) {
                if now.saturating_duration_since(*at) < OPEN_BACKOFF {
                    return Err(io::Error::new(
                        io::ErrorKind::NotConnected,
                        "corsair_axi: last open failed; waiting before retrying",
                    ));
                }
            }
        }
        let session = self.open(sysfs, tty, now)?;
        let page_due = now.saturating_duration_since(session.page_checked) >= PAGE_RECHECK;
        let result = if page_due {
            match session.read_register(PAGE, 1) {
                Ok(page) => {
                    if let Err(e) = page_is_zero(&page) {
                        // The PSU is on another page: back off the port as for
                        // a failed open, without blaming this register.
                        self.sessions.remove(tty);
                        self.open_failed.insert(tty.to_path_buf(), now);
                        return Err(e);
                    }
                    session.page_checked = now;
                    session.read_linear11(register)
                }
                // A lost or garbled reply: handled below like a failed reading.
                Err(e) => Err(e),
            }
        } else {
            session.read_linear11(register)
        };
        match result {
            Ok(value) => {
                self.register_failed.remove(&key);
                Ok(value)
            }
            Err(e) => {
                // A lost or garbled reply leaves the stream position unknown.
                self.sessions.remove(tty);
                let count = self.register_failed.get(&key).map_or(0, |f| f.count) + 1;
                self.register_failed.insert(key, Failure { at: now, count });
                Err(e)
            }
        }
    }
}

/// The sampler reads from one blocking worker at a time, so the lock is
/// uncontended; it exists to keep sessions across reads.
static SESSIONS: Mutex<Option<Sessions<SerialPort>>> = Mutex::new(None);

fn sessions<T>(f: impl FnOnce(&mut Sessions<SerialPort>) -> T) -> T {
    let mut guard = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(|| {
        Sessions::new(|sysfs: &Path, tty: &Path| {
            if !is_dongle(sysfs, tty) {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "corsair_axi: {} is no longer a Corsair Link dongle",
                        tty.display()
                    ),
                ));
            }
            SerialPort::open_exclusive(tty)
        })
    }))
}

/// The model of the PSU on `tty`, opening a session if none is open. `sysfs`
/// is where the tty is checked to still be a dongle before it is opened.
pub(crate) fn model(sysfs: &Path, tty: &Path) -> io::Result<String> {
    sessions(|s| s.model(sysfs, tty, Instant::now()))
}

/// Closes and forgets every port not in `ttys`.
pub(crate) fn retain(ttys: &[PathBuf]) {
    sessions(|s| s.retain(ttys))
}

/// A LINEAR11 register of the PSU on `tty`, in its PMBus unit.
pub(crate) fn read(sysfs: &Path, tty: &Path, register: u8) -> io::Result<f64> {
    sessions(|s| s.read(sysfs, tty, register, Instant::now()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// One captured line: `dir` is "tx" or "rx", `hex` the bytes, `note` what
    /// the capture script was doing.
    #[derive(serde::Deserialize)]
    struct Line {
        dir: String,
        hex: String,
        note: String,
    }

    fn lines(fixture: &str) -> Vec<(String, Vec<u8>, String)> {
        fixture
            .lines()
            .map(|l| {
                let l: Line = serde_json::from_str(l).unwrap();
                let bytes = (0..l.hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&l.hex[i..i + 2], 16).unwrap())
                    .collect();
                (l.dir, bytes, l.note)
            })
            .collect()
    }

    const HANDSHAKE: &str = include_str!("fixtures/corsair-axi-ax1500i-handshake.jsonl");
    const LOAD: &str = include_str!("fixtures/corsair-axi-ax1500i-load.jsonl");

    /// Plays captured replies back in order and records what is written.
    struct Replay {
        rx: VecDeque<u8>,
        written: Vec<u8>,
    }

    impl Replay {
        fn of(fixture: &str) -> Self {
            Self {
                rx: lines(fixture)
                    .into_iter()
                    .filter(|(dir, _, _)| dir == "rx")
                    .flat_map(|(_, bytes, _)| bytes)
                    .collect(),
                written: Vec::new(),
            }
        }
    }

    impl Port for Replay {
        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.written.extend_from_slice(bytes);
            Ok(())
        }
        fn read_byte(&mut self, _deadline: Instant) -> io::Result<u8> {
            self.rx
                .pop_front()
                .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "replay exhausted"))
        }
        fn discard_input(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn requests_match_the_capture() {
        let tx: Vec<Vec<u8>> = lines(HANDSHAKE)
            .into_iter()
            .filter(|(dir, _, _)| dir == "tx")
            .map(|(_, bytes, _)| bytes)
            .collect();
        assert_eq!(tx[0], encode(&[2]));
        assert_eq!(tx[1], encode(&[17, 2, 100, 0, 0, 0, 0]));
        assert_eq!(tx[2], encode(&[0]));
        assert_eq!(tx[3], encode(&[19, 3, 6, 1, 7, 7, MFR_MODEL]));
        assert_eq!(tx[4], encode(&[8, 7, 7]));
        assert_eq!(tx[5], encode(&[19, 3, 6, 1, 7, 1, PAGE]));
        assert_eq!(tx[7], encode(&[19, 3, 6, 1, 7, 2, READ_PIN]));
    }

    #[test]
    fn every_captured_reply_decodes() {
        let rx: Vec<(Vec<u8>, String)> = lines(HANDSHAKE)
            .into_iter()
            .filter(|(dir, _, _)| dir == "rx")
            .map(|(_, bytes, note)| (bytes, note))
            .collect();
        for (frame, note) in &rx {
            decode(frame).unwrap_or_else(|e| panic!("{note}: {e}"));
        }
        let name = decode(&rx[0].0).unwrap();
        assert!(String::from_utf8_lossy(&name).starts_with("USB to SMB Bridge"));
        // The dongle's name is far longer than a register reply: frames are
        // read to their terminator, not by count.
        assert!(rx[0].0.len() > 40);
    }

    #[test]
    fn a_session_over_the_capture_reads_the_model_and_registers() {
        let mut s = Session::open(Replay::of(HANDSHAKE)).unwrap();
        assert_eq!(s.model(), "AX1500i");
        // The capture then reads PIN, VIN, IIN, 0xee, temp2, temp1, fan per poll.
        let pin = s.read_linear11(READ_PIN).unwrap();
        let vin = s.read_linear11(READ_VIN).unwrap();
        let iin = s.read_linear11(READ_IIN).unwrap();
        assert!((150.0..250.0).contains(&pin), "{pin}");
        assert!((200.0..260.0).contains(&vin), "{vin}");
        assert!(
            ((vin * iin) - pin).abs() / pin < 0.03,
            "{vin} V x {iin} A vs {pin} W"
        );
        // The request bytes the session sent are the captured ones.
        let tx: Vec<u8> = lines(HANDSHAKE)
            .into_iter()
            .filter(|(dir, _, _)| dir == "tx")
            .flat_map(|(_, bytes, _)| bytes)
            .collect();
        assert_eq!(s.port.written[..], tx[..s.port.written.len()]);
    }

    #[test]
    fn input_power_agrees_with_voltage_times_current_across_a_load_step() {
        // Pair each poll's READ_PIN, READ_VIN and READ_IIN replies. This
        // capture read replies by count and lost alignment after the dongle's
        // long name, and it lacks the acknowledgement of the first PAGE
        // request; frames that do not decode to two bytes are skipped, as a
        // session would drop them.
        let mut poll: HashMap<&str, f64> = HashMap::new();
        let mut checked = 0;
        let (mut lo, mut hi) = (f64::MAX, 0f64);
        for (dir, bytes, note) in lines(LOAD) {
            if dir != "rx" || note.contains("(req)") {
                continue;
            }
            let Ok(data) = decode(&bytes) else { continue };
            if data.len() != 2 {
                continue;
            }
            let value = linear11(data[0], data[1]);
            for name in ["READ_PIN", "READ_VIN", "READ_IIN"] {
                if note.starts_with(name) {
                    poll.insert(name, value);
                }
            }
            if note.starts_with("READ_IIN") {
                if let (Some(&p), Some(&v), Some(&i)) = (
                    poll.get("READ_PIN"),
                    poll.get("READ_VIN"),
                    poll.get("READ_IIN"),
                ) {
                    if v > 100.0 {
                        assert!(((v * i) - p).abs() / p < 0.05, "{v} V x {i} A vs {p} W");
                        lo = lo.min(p);
                        hi = hi.max(p);
                        checked += 1;
                    }
                }
                poll.clear();
            }
        }
        assert!(checked > 40, "only {checked} polls");
        // Idle near 190 W, and the capture's CPU load near 370 W.
        assert!(lo < 200.0 && hi > 350.0, "{lo}..{hi}");
    }

    #[test]
    fn linear11_values() {
        assert_eq!(super::linear11(0, 0), 0.0);
        // exponent -1, mantissa 3
        let raw: u16 = (0x1f << 11) | 3;
        assert_eq!(super::linear11(raw as u8, (raw >> 8) as u8), 1.5);
        // exponent 0, mantissa -1
        assert_eq!(super::linear11(0xff, 0x07), -1.0);
        // exponent 2, mantissa 5
        let raw: u16 = (2 << 11) | 5;
        assert_eq!(super::linear11(raw as u8, (raw >> 8) as u8), 20.0);
    }

    #[test]
    fn malformed_frames_are_errors() {
        let good = decode(&[0xa8, 0x56, 0x55, 0x00]).unwrap();
        assert_eq!(good, [1]);
        for bad in [
            &[][..],
            &[0x54, 0x56, 0x55, 0x00], // a request start byte
            &[0xa8, 0x56, 0x55],       // no terminator
            &[0xa8, 0x56, 0x00],       // half a pair
            &[0xa8, 0x56, 0x12, 0x00], // outside the code table
        ] {
            assert!(decode(bad).is_err(), "{bad:x?} decoded");
        }
    }

    #[test]
    fn a_lost_reply_is_an_error_not_a_hang() {
        let mut replay = Replay::of(HANDSHAKE);
        replay.rx.truncate(10); // part of the dongle's name, then nothing
        assert_eq!(
            Session::open(replay).err().map(|e| e.kind()),
            Some(io::ErrorKind::TimedOut)
        );
    }

    /// A PSU behind a dongle that answers each request as the AX1500i did,
    /// with knobs for the failures under test.
    #[derive(Default)]
    struct Psu {
        page: u8,
        /// Registers whose read request gets no reply.
        silent: Vec<u8>,
        /// Registers whose next read request only gets no reply.
        silent_once: Vec<u8>,
        /// Registers whose acknowledgement arrives just after the reader has
        /// timed out: a late reply left on the line.
        late: Vec<u8>,
        refuse_open: bool,
        opens: usize,
        reads: HashMap<u8, usize>,
        /// Bytes on the line, shared across opens of the port: what one
        /// session leaves unread, the next open sees unless it discards it.
        rx: VecDeque<u8>,
        held: Vec<u8>,
    }

    struct Scripted {
        psu: std::sync::Arc<std::sync::Mutex<Psu>>,
        pending: Option<(u8, u8)>,
    }

    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut f = vec![REPLY_START];
        for &b in payload {
            f.push(CODES[(b & 0xf) as usize]);
            f.push(CODES[(b >> 4) as usize]);
        }
        f.push(TERMINATOR);
        f
    }

    impl Port for Scripted {
        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            assert_eq!(bytes[0], REQUEST_START);
            let request: Vec<u8> = bytes[1..bytes.len() - 1]
                .chunks(2)
                .map(|p| nibble(p[0]).unwrap() | nibble(p[1]).unwrap() << 4)
                .collect();
            let psu = self.psu.clone();
            let mut psu = psu.lock().unwrap();
            let reply = |psu: &mut Psu, payload: &[u8]| psu.rx.extend(frame(payload));
            match request.as_slice() {
                [2] => reply(&mut psu, b"USB to SMB Bridge (Firmware by Ross Fosler)\0"),
                [17, ..] | [0] => reply(&mut psu, &[0, 9]),
                [19, 3, 6, 1, 7, len, register] => {
                    *psu.reads.entry(*register).or_default() += 1;
                    let once = psu.silent_once.iter().position(|r| r == register);
                    if let Some(at) = once {
                        psu.silent_once.remove(at);
                    } else if psu.late.contains(register) {
                        psu.held = frame(&[]);
                    } else if !psu.silent.contains(register) {
                        self.pending = Some((*register, *len));
                        reply(&mut psu, &[]);
                    }
                }
                [8, 7, len] => {
                    let (register, _) = self.pending.take().expect("a pending read");
                    let data: Vec<u8> = match register {
                        MFR_MODEL => b"AX1500i".to_vec(),
                        PAGE => vec![psu.page],
                        _ => vec![0xbb, 0x00], // LINEAR11 187
                    };
                    assert_eq!(data.len(), usize::from(*len));
                    reply(&mut psu, &data);
                }
                other => panic!("unexpected request {other:?}"),
            }
            Ok(())
        }
        fn read_byte(&mut self, _deadline: Instant) -> io::Result<u8> {
            let mut psu = self.psu.lock().unwrap();
            psu.rx.pop_front().ok_or_else(|| {
                let held = std::mem::take(&mut psu.held);
                psu.rx.extend(held);
                io::Error::new(io::ErrorKind::TimedOut, "timed out")
            })
        }
        fn discard_input(&mut self) -> io::Result<()> {
            self.psu.lock().unwrap().rx.clear();
            Ok(())
        }
    }

    fn scripted(psu: Psu) -> (std::sync::Arc<std::sync::Mutex<Psu>>, Sessions<Scripted>) {
        let psu = std::sync::Arc::new(std::sync::Mutex::new(psu));
        let shared = psu.clone();
        let sessions = Sessions::new(move |_: &Path, _: &Path| {
            let mut state = shared.lock().unwrap();
            state.opens += 1;
            if state.refuse_open {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "no reply"));
            }
            Ok(Scripted {
                psu: shared.clone(),
                pending: None,
            })
        });
        (psu, sessions)
    }

    const TTY: &str = "/dev/ttyUSB0";
    const SYS: &str = "/nonexistent-sysfs";
    const SWEEP: [u8; 6] = [
        READ_PIN,
        READ_VIN,
        READ_IIN,
        READ_TEMPERATURE_1,
        READ_TEMPERATURE_2,
        READ_FAN_SPEED_1,
    ];

    #[test]
    fn the_scripted_psu_reads_like_the_capture() {
        let (_, mut sessions) = scripted(Psu::default());
        let now = Instant::now();
        assert_eq!(
            sessions.model(Path::new(SYS), Path::new(TTY), now).unwrap(),
            "AX1500i"
        );
        assert_eq!(
            sessions
                .read(Path::new(SYS), Path::new(TTY), READ_PIN, now)
                .unwrap(),
            187.0
        );
    }

    #[test]
    fn a_register_that_never_answers_backs_off_and_the_rest_keep_reading() {
        let (psu, mut sessions) = scripted(Psu {
            silent: vec![READ_TEMPERATURE_2],
            ..Default::default()
        });
        let t0 = Instant::now();
        let tty = Path::new(TTY);
        sessions.model(Path::new(SYS), tty, t0).unwrap();
        let sweeps = 60; // five minutes at 5 s
        for k in 0..sweeps {
            let now = t0 + Duration::from_secs(5 * k);
            for register in SWEEP {
                let read = sessions.read(Path::new(SYS), tty, register, now);
                assert_eq!(
                    read.is_ok(),
                    register != READ_TEMPERATURE_2,
                    "sweep {k} {register:#x}"
                );
            }
        }
        let psu = psu.lock().unwrap();
        let tries = psu.reads[&READ_TEMPERATURE_2];
        // 1, 2, 4, 8, 16, 32 s, then every 60 s: 0, 5, 10, 15, 25, 45, 80, 140, 200, 260 s.
        assert_eq!(
            tries, 10,
            "the silent register was tried {tries} times in {sweeps} sweeps"
        );
        assert_eq!(psu.reads[&READ_FAN_SPEED_1], sweeps as usize);
        // One reopen per try, not one per sweep.
        assert!(psu.opens <= 1 + tries, "{} opens", psu.opens);
    }

    #[test]
    fn a_register_that_answers_again_is_read_again_from_the_next_sweep() {
        let (psu, mut sessions) = scripted(Psu {
            silent: vec![READ_PIN],
            ..Default::default()
        });
        let t0 = Instant::now();
        let tty = Path::new(TTY);
        assert!(sessions.read(Path::new(SYS), tty, READ_PIN, t0).is_err());
        psu.lock().unwrap().silent.clear();
        assert!(
            sessions.read(Path::new(SYS), tty, READ_PIN, t0).is_err(),
            "held back within the second"
        );
        assert!(sessions
            .read(Path::new(SYS), tty, READ_PIN, t0 + Duration::from_secs(1))
            .is_ok());
        assert!(sessions.register_failed.is_empty());
    }

    #[test]
    fn a_failed_open_holds_back_readings_but_not_discovery() {
        let (psu, mut sessions) = scripted(Psu {
            refuse_open: true,
            ..Default::default()
        });
        let t0 = Instant::now();
        let tty = Path::new(TTY);
        assert!(sessions.model(Path::new(SYS), tty, t0).is_err());
        for k in 0..11 {
            for register in SWEEP {
                assert!(sessions
                    .read(
                        Path::new(SYS),
                        tty,
                        register,
                        t0 + Duration::from_secs(5 * k)
                    )
                    .is_err());
            }
        }
        assert_eq!(
            psu.lock().unwrap().opens,
            1,
            "a reading retried the open within the backoff"
        );
        // Discovery probes on its own schedule, whatever the backoff.
        assert!(sessions
            .model(Path::new(SYS), tty, t0 + Duration::from_secs(55))
            .is_err());
        assert_eq!(psu.lock().unwrap().opens, 2);
        psu.lock().unwrap().refuse_open = false;
        assert!(sessions
            .read(
                Path::new(SYS),
                tty,
                READ_PIN,
                t0 + Duration::from_secs(55) + OPEN_BACKOFF
            )
            .is_ok());
    }

    #[test]
    fn a_page_changed_by_another_tool_is_caught_within_a_second() {
        let (psu, mut sessions) = scripted(Psu::default());
        let t0 = Instant::now();
        let tty = Path::new(TTY);
        sessions.model(Path::new(SYS), tty, t0).unwrap();
        psu.lock().unwrap().page = 2;
        // Checked at open; not again within the second.
        assert!(sessions.read(Path::new(SYS), tty, READ_PIN, t0).is_ok());
        let err = sessions
            .read(Path::new(SYS), tty, READ_VIN, t0 + PAGE_RECHECK)
            .expect_err("page 2 is refused");
        assert!(err.to_string().contains("page"), "{err}");
        // Nothing switched it back.
        assert_eq!(psu.lock().unwrap().page, 2);
        // The port backs off as after a failed open; no register is blamed.
        let opens = psu.lock().unwrap().opens;
        assert!(sessions
            .read(Path::new(SYS), tty, READ_IIN, t0 + Duration::from_secs(30))
            .is_err());
        assert_eq!(psu.lock().unwrap().opens, opens);
        assert!(sessions.register_failed.is_empty());
        assert!(sessions.sessions.is_empty(), "the port is closed");
    }

    #[test]
    fn a_lost_page_reply_costs_one_register_not_the_port() {
        let (psu, mut sessions) = scripted(Psu::default());
        let t0 = Instant::now();
        let tty = Path::new(TTY);
        sessions.model(Path::new(SYS), tty, t0).unwrap();
        psu.lock().unwrap().silent_once.push(PAGE);
        let t1 = t0 + PAGE_RECHECK;
        assert!(sessions.read(Path::new(SYS), tty, READ_PIN, t1).is_err());
        assert!(sessions.open_failed.is_empty(), "no port backoff");
        // The next register reopens and reads.
        assert_eq!(
            sessions.read(Path::new(SYS), tty, READ_VIN, t1).unwrap(),
            187.0
        );
    }

    #[test]
    fn a_late_reply_after_a_failure_is_not_read_as_the_next_one() {
        // READ_VIN's acknowledgement arrives as the session gives up on it.
        // Dropping the session and discarding input at the next open keep
        // that frame from pairing with the next request.
        let (psu, mut sessions) = scripted(Psu {
            late: vec![READ_VIN],
            ..Default::default()
        });
        let t0 = Instant::now();
        let tty = Path::new(TTY);
        sessions.model(Path::new(SYS), tty, t0).unwrap();
        assert!(sessions.read(Path::new(SYS), tty, READ_VIN, t0).is_err());
        psu.lock().unwrap().late.clear();
        for register in [READ_PIN, READ_IIN, READ_FAN_SPEED_1] {
            assert_eq!(
                sessions.read(Path::new(SYS), tty, register, t0).unwrap(),
                187.0,
                "{register:#x}"
            );
        }
    }

    #[test]
    fn register_backoff_doubles_to_the_cap_without_overflow() {
        let at = Instant::now();
        let wait = |count| Failure { at, count }.register_wait();
        assert_eq!(wait(1), Duration::from_secs(1));
        assert_eq!(wait(2), Duration::from_secs(2));
        assert_eq!(wait(6), Duration::from_secs(32));
        assert_eq!(wait(7), MAX_REGISTER_BACKOFF);
        assert_eq!(wait(40), MAX_REGISTER_BACKOFF);
        assert_eq!(wait(u32::MAX), MAX_REGISTER_BACKOFF);
    }

    #[test]
    fn a_frame_without_a_terminator_is_cut_off() {
        /// Sends code bytes forever and never a terminator.
        struct Endless;
        impl Port for Endless {
            fn write_all(&mut self, _: &[u8]) -> io::Result<()> {
                Ok(())
            }
            fn read_byte(&mut self, _: Instant) -> io::Result<u8> {
                Ok(0x55)
            }
            fn discard_input(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let err = Session::open(Endless).err().expect("refused");
        assert!(err.to_string().contains("too long"), "{err}");
    }

    #[test]
    fn sessions_for_ports_discovery_no_longer_finds_are_closed() {
        let (psu, mut sessions) = scripted(Psu::default());
        let t0 = Instant::now();
        let tty = Path::new(TTY);
        sessions.model(Path::new(SYS), tty, t0).unwrap();
        sessions.retain(&[PathBuf::from(TTY)]);
        assert_eq!(sessions.sessions.len(), 1);
        // Failures for a vanished port go too.
        sessions
            .open_failed
            .insert(PathBuf::from("/dev/ttyUSB9"), t0);
        sessions.register_failed.insert(
            (PathBuf::from("/dev/ttyUSB9"), READ_PIN),
            Failure { at: t0, count: 1 },
        );
        sessions.retain(&[]);
        assert!(sessions.sessions.is_empty());
        assert!(sessions.open_failed.is_empty());
        assert!(sessions.register_failed.is_empty());
        // A device that later appears at the same number is asked its model.
        sessions.model(Path::new(SYS), tty, t0).unwrap();
        assert_eq!(psu.lock().unwrap().opens, 2);
    }

    #[test]
    fn is_dongle_follows_the_tty_to_its_usb_device() {
        let root = tempfile::tempdir().unwrap();
        let usb = root.path().join("devices/pci0000:00/usb1");
        for (name, tty, vendor, product) in [
            ("1-13", "ttyUSB0", "1b1c", "1c02"),
            ("1-7", "ttyUSB1", "10c4", "ea60"),
            ("1-8", "ttyUSB2", "1b1c", "1c0a"),
        ] {
            // As on hv02: the class link's `device` is the usb-serial port
            // under the interface, `.../1-13/1-13:1.0/ttyUSB0`.
            let port = usb.join(name).join(format!("{name}:1.0")).join(tty);
            std::fs::create_dir_all(port.join("tty").join(tty)).unwrap();
            std::fs::write(usb.join(name).join("idVendor"), format!("{vendor}\n")).unwrap();
            std::fs::write(usb.join(name).join("idProduct"), format!("{product}\n")).unwrap();
            let class = root.path().join("class/tty").join(tty);
            std::fs::create_dir_all(&class).unwrap();
            std::os::unix::fs::symlink(&port, class.join("device")).unwrap();
        }
        assert!(is_dongle(root.path(), Path::new("/dev/ttyUSB0")));
        // A plain CP210x (another vendor's adapter) now at the number the dongle had.
        assert!(!is_dongle(root.path(), Path::new("/dev/ttyUSB1")));
        // Another Corsair product.
        assert!(!is_dongle(root.path(), Path::new("/dev/ttyUSB2")));
        assert!(!is_dongle(root.path(), Path::new("/dev/ttyUSB3")));
    }

    /// A pseudo-terminal: the master fd and the slave's path.
    fn pty() -> (OwnedFd, PathBuf) {
        // SAFETY: plain libc calls; the fd is owned on success.
        unsafe {
            let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            assert!(master >= 0, "{}", io::Error::last_os_error());
            assert_eq!(libc::grantpt(master), 0);
            assert_eq!(libc::unlockpt(master), 0);
            let mut name = [0 as libc::c_char; 128];
            assert_eq!(libc::ptsname_r(master, name.as_mut_ptr(), name.len()), 0);
            let path = std::ffi::CStr::from_ptr(name.as_ptr())
                .to_str()
                .unwrap()
                .to_string();
            let path = PathBuf::from(path);
            // Pty numbers are reused across parallel tests: wait out a close
            // another test left pending on this path.
            let started = Instant::now();
            while closing().contains(&path) && started.elapsed() < Duration::from_secs(2) {
                std::thread::sleep(Duration::from_millis(5));
            }
            (OwnedFd::from_raw_fd(master), path)
        }
    }

    #[test]
    fn a_silent_port_times_out_at_the_deadline() {
        let (_master, slave) = pty();
        let mut port = SerialPort::open_exclusive(&slave).unwrap();
        let started = Instant::now();
        let err = port
            .read_byte(started + Duration::from_millis(100))
            .expect_err("nothing was sent");
        let waited = started.elapsed();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(
            waited >= Duration::from_millis(100) && waited < Duration::from_secs(2),
            "{waited:?}"
        );
    }

    #[test]
    fn a_hangup_is_an_error_before_the_deadline() {
        let (master, slave) = pty();
        let mut port = SerialPort::open(&slave).unwrap();
        drop(master);
        let started = Instant::now();
        assert!(port.read_byte(started + Duration::from_secs(5)).is_err());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_port_another_process_has_locked_is_refused() {
        // A holder that takes only the flock, with no TIOCEXCL, so only the
        // lock can refuse the open.
        let (_master, slave) = pty();
        let c = std::ffi::CString::new(slave.to_str().unwrap()).unwrap();
        // SAFETY: c is a valid path; the fd is owned below.
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NOCTTY) };
        assert!(fd >= 0);
        // SAFETY: fd was just opened.
        let _holder = unsafe { OwnedFd::from_raw_fd(fd) };
        assert_eq!(unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) }, 0);
        let err = SerialPort::open_exclusive(&slave).err().expect("refused");
        assert!(err.to_string().contains("locked"), "{err}");
    }

    #[test]
    fn a_port_another_process_has_set_exclusive_is_refused() {
        // A holder that sets TIOCEXCL and takes no lock. As non-root the open
        // itself fails with EBUSY; as root (the packaged agent) it succeeds,
        // and the TIOCGEXCL check refuses the port.
        let (_master, slave) = pty();
        let c = std::ffi::CString::new(slave.to_str().unwrap()).unwrap();
        // SAFETY: c is a valid path; the fd is owned below.
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NOCTTY) };
        assert!(fd >= 0);
        // SAFETY: fd was just opened.
        let _holder = unsafe { OwnedFd::from_raw_fd(fd) };
        assert_eq!(unsafe { libc::ioctl(fd, libc::TIOCEXCL) }, 0);
        assert!(SerialPort::open_exclusive(&slave).is_err());
    }

    #[test]
    fn a_port_set_exclusive_after_it_was_opened_is_refused() {
        // Opened before the holder set TIOCEXCL, so the open succeeds even as
        // non-root and only the TIOCGEXCL check can refuse it.
        let (_master, slave) = pty();
        let fd = SerialPort::open_fd(&slave).unwrap();
        let holder = SerialPort::open_fd(&slave).unwrap();
        assert_eq!(
            unsafe { libc::ioctl(holder.as_raw_fd(), libc::TIOCEXCL) },
            0
        );
        let err = SerialPort::claim(fd, &slave).err().expect("refused");
        assert_eq!(err.kind(), io::ErrorKind::ResourceBusy, "{err}");
    }

    #[test]
    fn a_refused_port_keeps_its_settings() {
        let (_master, slave) = pty();
        let holder = SerialPort::open_fd(&slave).unwrap();
        // SAFETY: plain termios calls on an open fd.
        let speed = unsafe {
            let mut tio: libc::termios = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(holder.as_raw_fd(), &mut tio), 0);
            libc::cfsetispeed(&mut tio, libc::B9600);
            libc::cfsetospeed(&mut tio, libc::B9600);
            assert_eq!(libc::tcsetattr(holder.as_raw_fd(), libc::TCSANOW, &tio), 0);
            assert_eq!(
                libc::flock(holder.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB),
                0
            );
            assert!(SerialPort::open_exclusive(&slave).is_err());
            assert_eq!(libc::tcgetattr(holder.as_raw_fd(), &mut tio), 0);
            libc::cfgetispeed(&tio)
        };
        assert_eq!(speed, libc::B9600);
    }

    #[test]
    fn claiming_a_port_does_not_set_tiocexcl() {
        // The kernel would keep it after this agent's close, including at exit,
        // while another process has the port open.
        let (_master, slave) = pty();
        let holder = SerialPort::open_fd(&slave).unwrap();
        let _port = SerialPort::open_exclusive(&slave).unwrap();
        let mut excl: libc::c_int = 1;
        // SAFETY: holder is open; TIOCGEXCL writes one c_int.
        assert_eq!(
            unsafe { libc::ioctl(holder.as_raw_fd(), libc::TIOCGEXCL, &mut excl) },
            0
        );
        assert_eq!(excl, 0);
    }

    #[test]
    fn a_port_whose_close_is_pending_is_refused_without_opening_it() {
        // A path that cannot be opened: any refusal but ClosePending would
        // mean the open was attempted.
        let tty = PathBuf::from("/nonexistent/ttyUSB-closing");
        closing().push(tty.clone());
        let started = Instant::now();
        let err = SerialPort::open_exclusive(&tty).err().expect("refused");
        let waited = started.elapsed();
        closing().retain(|p| *p != tty);
        assert!(is_close_pending(&err), "{err}");
        assert!(
            waited >= CLOSE_WAIT && waited < Duration::from_secs(1),
            "{waited:?}"
        );
    }

    #[test]
    fn an_open_waits_out_a_short_close() {
        let tty = PathBuf::from("/nonexistent/ttyUSB-short-close");
        closing().push(tty.clone());
        let done = tty.clone();
        let closer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(2));
            closing().retain(|p| *p != done);
        });
        // Past the wait, the open is attempted and fails on the missing path.
        let err = SerialPort::open_exclusive(&tty)
            .err()
            .expect("no such path");
        closer.join().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::NotFound, "{err}");
    }

    #[test]
    fn a_long_pending_close_costs_one_wait_per_sweep() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = calls.clone();
        let mut sessions: Sessions<Scripted> = Sessions::new(move |_: &Path, _: &Path| {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(io::Error::new(io::ErrorKind::ResourceBusy, ClosePending))
        });
        let t0 = Instant::now();
        let tty = Path::new(TTY);
        for k in 0..6 {
            let now = t0 + Duration::from_secs(5 * k);
            for register in SWEEP {
                assert!(sessions.read(Path::new(SYS), tty, register, now).is_err());
            }
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 6);
        assert!(sessions.open_failed.is_empty());
    }

    #[test]
    fn a_pending_close_does_not_back_off_the_port() {
        let psu = std::sync::Arc::new(std::sync::Mutex::new(Psu::default()));
        let shared = psu.clone();
        let refused = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let once = refused.clone();
        let mut sessions = Sessions::new(move |_: &Path, _: &Path| {
            if !once.swap(true, std::sync::atomic::Ordering::SeqCst) {
                return Err(io::Error::new(io::ErrorKind::ResourceBusy, ClosePending));
            }
            Ok(Scripted {
                psu: shared.clone(),
                pending: None,
            })
        });
        let t0 = Instant::now();
        let tty = Path::new(TTY);
        assert!(sessions.read(Path::new(SYS), tty, READ_PIN, t0).is_err());
        assert!(sessions.open_failed.is_empty());
        // The next sweep opens and reads; no 60 s backoff.
        assert_eq!(
            sessions
                .read(Path::new(SYS), tty, READ_VIN, t0 + Duration::from_secs(5))
                .unwrap(),
            187.0
        );
        drop(psu);
    }

    #[test]
    fn a_dropped_port_is_claimable_once_its_close_returns() {
        let (_master, slave) = pty();
        drop(SerialPort::open_exclusive(&slave).unwrap());
        let started = Instant::now();
        while closing().contains(&slave) {
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "close never finished"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(SerialPort::open_exclusive(&slave).is_ok());
    }

    #[test]
    fn a_port_held_by_another_session_is_refused() {
        let (_master, slave) = pty();
        let _held = SerialPort::open_exclusive(&slave).unwrap();
        assert!(SerialPort::open_exclusive(&slave).is_err());
    }

    #[test]
    fn a_nonzero_pmbus_page_is_refused_not_switched() {
        let mut replay = Replay::of(HANDSHAKE);
        // Replace the PAGE reply (a8 5555 00 = [0]) with [1].
        let rx: Vec<u8> = replay.rx.iter().copied().collect();
        let page_reply = [0xa8, 0x55, 0x55, 0x00];
        let at = rx
            .windows(4)
            .position(|w| w == page_reply)
            .expect("the capture's PAGE reply");
        replay.rx[at + 1] = 0x56;
        let err = Session::open(replay).err().expect("refused");
        assert!(err.to_string().contains("page"), "{err}");
    }
}
