//! Single instance (Notepad behaviour): launching the app while it is
//! already running hands the files to the running window and exits.
//!
//! The protocol, the "who is first" election and the server loop are
//! generic over [`Transport`] / [`Listener`] so they are unit-tested on any
//! OS with in-memory fakes.  The Windows named-pipe transport is in [`pipe`].
//!
//! Design (Windows):
//! - One pipe per user session: `\\.\pipe\<app id>-s<session>-<user>`.
//!   Pipe names are machine-wide, hence the session id and user name.
//! - Election: the first process to create the pipe with
//!   `FILE_FLAG_FIRST_PIPE_INSTANCE` is the primary.  That create is atomic,
//!   so two launches at the same moment cannot both win, and the pipe
//!   vanishes with its process, so a crashed primary leaves no stale lock
//!   (no separate mutex needed).
//! - A later launch connects, grants the primary the right to take the
//!   foreground (`AllowSetForegroundWindow`), sends its paths (made absolute
//!   against its own working directory) and waits for a one-byte ack.  Every
//!   wait is bounded ([`Timing`]); if anything fails it opens its own window.
//! - The primary accepts on a background thread, gives each connection its
//!   own short-lived thread, and hands requests to the UI thread through a
//!   channel plus `request_repaint` (restoring the window first if it is
//!   minimized, since a minimized window may not run frames).

// Only the Windows glue uses this outside tests; it is compiled everywhere
// so the tests run on the Linux dev host.
#![cfg_attr(not(windows), allow(dead_code))]

use std::{
    fmt::Write as _,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, mpsc},
    thread,
    time::{Duration, Instant},
};

use eframe::egui;

use crate::app_identity::APP_ID;

/// Frame header: protocol name and version.
const MAGIC: [u8; 8] = *b"rustdwn1";
/// Largest accepted payload (thousands of long paths).
const MAX_PAYLOAD: usize = 1 << 20;
const ACK_OK: u8 = b'+';
const ACK_ERR: u8 = b'-';

/// What a later launch asks the running window to do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct OpenRequest {
    /// Absolute paths to open (or focus, if already open).  Empty: open a
    /// new empty tab.
    pub(crate) paths: Vec<PathBuf>,
}

impl OpenRequest {
    /// Request for command-line `paths`, made absolute against the sender's
    /// working directory `cwd` (the receiver's is different).
    pub(crate) fn from_args(paths: impl IntoIterator<Item = PathBuf>, cwd: &Path) -> Self {
        let paths = paths
            .into_iter()
            .map(|path| {
                if path.is_absolute() {
                    path
                } else {
                    cwd.join(path)
                }
            })
            .collect();
        Self { paths }
    }

    /// One frame: [`MAGIC`], payload length (`u32` little endian), payload.
    /// The payload is each path in UTF-8 followed by a NUL.  `None` when a
    /// path cannot be sent (not Unicode, or not absolute).
    pub(crate) fn encode(&self) -> Option<Vec<u8>> {
        let mut payload = Vec::new();
        for path in &self.paths {
            let text = path.to_str()?;
            if !path.is_absolute() || text.contains('\0') {
                return None;
            }
            payload.extend_from_slice(text.as_bytes());
            payload.push(0);
        }
        if payload.len() > MAX_PAYLOAD {
            return None;
        }
        let len = u32::try_from(payload.len()).ok()?;
        let mut frame = Vec::with_capacity(MAGIC.len() + 4 + payload.len());
        frame.extend_from_slice(&MAGIC);
        frame.extend_from_slice(&len.to_le_bytes());
        frame.extend_from_slice(&payload);
        Some(frame)
    }

    /// Read one frame written by [`Self::encode`].
    pub(crate) fn read_from(reader: &mut impl Read) -> io::Result<Self> {
        let mut magic = [0; MAGIC.len()];
        reader.read_exact(&mut magic)?;
        if magic != MAGIC {
            return Err(invalid_data("not an open-files request"));
        }
        let mut len = [0; 4];
        reader.read_exact(&mut len)?;
        let len = usize::try_from(u32::from_le_bytes(len)).unwrap_or(usize::MAX);
        if len > MAX_PAYLOAD {
            return Err(invalid_data("request too large"));
        }
        let mut payload = vec![0; len];
        reader.read_exact(&mut payload)?;
        Self::decode_payload(&payload).ok_or_else(|| invalid_data("malformed request"))
    }

    fn decode_payload(payload: &[u8]) -> Option<Self> {
        if payload.is_empty() {
            return Some(Self::default());
        }
        let text = std::str::from_utf8(payload).ok()?;
        let paths: Vec<PathBuf> = text
            .strip_suffix('\0')?
            .split('\0')
            .map(PathBuf::from)
            .collect();
        paths
            .iter()
            .all(|path| path.is_absolute())
            .then_some(Self { paths })
    }
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Pipe name for one user session, e.g. `\\.\pipe\<app id>-s1-alice`.
pub(crate) fn pipe_name(session: Option<u32>, user: Option<&str>) -> String {
    let mut name = format!(r"\\.\pipe\{APP_ID}");
    if let Some(session) = session {
        let _ = write!(name, "-s{session}");
    }
    if let Some(user) = user.filter(|user| !user.is_empty()) {
        name.push('-');
        name.extend(user.chars().take(64).map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        }));
    }
    name
}

/// How long a launch waits before giving up and opening its own window.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    /// Total time to reach the running window while it is busy or starting.
    pub(crate) connect: Duration,
    /// Time for sending the request and receiving the ack.
    pub(crate) exchange: Duration,
    /// Pause between connection attempts.
    pub(crate) retry: Duration,
}

impl Timing {
    pub(crate) const DEFAULT: Self = Self {
        connect: Duration::from_secs(2),
        exchange: Duration::from_secs(3),
        retry: Duration::from_millis(25),
    };
}

/// Result of trying to reach the running window.
pub(crate) enum Connect<C> {
    Connected(C),
    /// Nobody is listening.
    NoServer,
    /// A server exists but has no free instance right now; try again.
    Busy,
    /// Unusable (e.g. a pipe owned by another user).
    Failed,
}

/// Accepts connections from later launches (server side).
pub(crate) trait Listener: Send + 'static {
    type Conn: Read + Write + Send + 'static;
    /// Block until the next client connects.  An error ends the server.
    fn accept(&mut self) -> io::Result<Self::Conn>;
}

/// The local IPC channel between launches.
pub(crate) trait Transport {
    type Listener: Listener;
    /// Connect to the running window (client side).
    fn connect(&self) -> Connect<<Self::Listener as Listener>::Conn>;
    /// Start listening if nobody else is; `Ok(None)` when another process
    /// already owns the name.
    fn listen_first(&self) -> io::Result<Option<Self::Listener>>;
}

/// Outcome of the startup election.
pub(crate) enum Election<L> {
    /// The running window took the request: exit.
    Forwarded,
    /// This process is the primary instance; serve later launches.
    Primary(L),
    /// Open a window of our own without serving anyone.
    Standalone,
}

/// Decide at startup whether to forward `request` (`None` for
/// `--new-window`: never forward) or to become the primary instance.
pub(crate) fn elect<T: Transport>(
    transport: &T,
    request: Option<&OpenRequest>,
    timing: Timing,
) -> Election<T::Listener> {
    let frame = request.and_then(OpenRequest::encode);
    let deadline = Instant::now() + timing.connect;
    loop {
        if let Some(frame) = &frame {
            match transport.connect() {
                Connect::Connected(conn) => {
                    return match exchange(conn, frame.clone(), timing.exchange) {
                        Ok(()) => Election::Forwarded,
                        Err(_) => Election::Standalone,
                    };
                }
                Connect::Busy if Instant::now() < deadline => {
                    thread::sleep(timing.retry);
                    continue;
                }
                Connect::Busy | Connect::Failed => return Election::Standalone,
                Connect::NoServer => {}
            }
        }
        match transport.listen_first() {
            Ok(Some(listener)) => return Election::Primary(listener),
            // Another launch won the race a moment ago: forward to it.
            Ok(None) if frame.is_some() && Instant::now() < deadline => {
                thread::sleep(timing.retry);
            }
            Ok(None) | Err(_) => return Election::Standalone,
        }
    }
}

/// Send `frame` over `conn` and wait for the ack.  The I/O runs on a helper
/// thread so that a hung primary cannot hang this launch past `timeout`.
pub(crate) fn exchange<C: Read + Write + Send + 'static>(
    conn: C,
    frame: Vec<u8>,
    timeout: Duration,
) -> io::Result<()> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name(format!("{APP_ID}-forward"))
        .spawn(move || {
            let mut conn = conn;
            let mut ack = [0];
            let result = conn
                .write_all(&frame)
                .and_then(|()| conn.read_exact(&mut ack))
                .map(|()| ack[0]);
            let _ = tx.send(result);
        })?;
    match rx.recv_timeout(timeout) {
        Ok(Ok(ACK_OK)) => Ok(()),
        Ok(Ok(_)) => Err(io::Error::other("request rejected")),
        Ok(Err(err)) => Err(err),
        Err(_) => Err(io::ErrorKind::TimedOut.into()),
    }
}

/// Read one request from `conn`, deliver it, and answer with an ack: OK
/// only if the window took the request, so the sender opens its own window
/// otherwise (e.g. when this one is closing).
fn handle_connection<C: Read + Write>(mut conn: C, deliver: &impl Fn(OpenRequest) -> bool) {
    let delivered = OpenRequest::read_from(&mut conn).is_ok_and(deliver);
    let ack = if delivered { ACK_OK } else { ACK_ERR };
    let _ = conn.write_all(&[ack]).and_then(|()| conn.flush());
}

/// Accept connections until the listener fails.  Each connection gets its
/// own thread, so a client that never sends only blocks that thread.
fn serve<L: Listener>(
    mut listener: L,
    deliver: impl Fn(OpenRequest) -> bool + Clone + Send + 'static,
) {
    while let Ok(conn) = listener.accept() {
        let deliver = deliver.clone();
        let _ = thread::Builder::new()
            .name(format!("{APP_ID}-instance-client"))
            .spawn(move || handle_connection(conn, &deliver));
    }
}

/// The primary window as seen from the server thread.
struct UiHandle {
    ctx: egui::Context,
    /// Native window handle (`HWND`), 0 if unknown.  Only Windows restores
    /// minimized windows.
    #[cfg_attr(not(windows), allow(dead_code))]
    hwnd: isize,
}

impl UiHandle {
    fn wake(&self) {
        #[cfg(windows)]
        crate::win_ffi::restore_if_minimized(self.hwnd);
        self.ctx.request_repaint();
    }
}

/// Requests from later launches, received by the primary instance.
pub(crate) struct Inbox {
    pub(crate) requests: mpsc::Receiver<OpenRequest>,
    ui: Arc<OnceLock<UiHandle>>,
}

impl Inbox {
    /// Connect the server to the window once it exists.  Requests that
    /// arrived earlier wait in the channel for the first frame.
    pub(crate) fn attach_ui(&self, ctx: egui::Context, hwnd: isize) {
        let _ = self.ui.set(UiHandle { ctx, hwnd });
    }
}

/// Serve later launches on a background thread.
pub(crate) fn spawn_server<L: Listener>(listener: L) -> io::Result<Inbox> {
    let (tx, requests) = mpsc::channel();
    let ui: Arc<OnceLock<UiHandle>> = Arc::default();
    let server_ui = Arc::clone(&ui);
    // Fails once the app (which owns the receiver) is gone.
    let deliver = move |request: OpenRequest| {
        let queued = tx.send(request).is_ok();
        if queued && let Some(ui) = server_ui.get() {
            ui.wake();
        }
        queued
    };
    thread::Builder::new()
        .name(format!("{APP_ID}-instance"))
        .spawn(move || serve(listener, deliver))?;
    Ok(Inbox { requests, ui })
}

/// Named-pipe transport (Windows).
#[cfg(windows)]
pub(crate) mod pipe {
    use std::{
        fs::{File, OpenOptions},
        io::{self, Read, Write},
    };

    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_NO_DATA, ERROR_PIPE_BUSY};

    use super::{Connect, Listener, Transport, pipe_name};
    use crate::win_ffi;

    fn is_os_error(err: &io::Error, code: u32) -> bool {
        err.raw_os_error() == Some(code.cast_signed())
    }

    pub(crate) struct PipeTransport {
        name: String,
    }

    impl PipeTransport {
        /// The pipe of this user's session.
        pub(crate) fn for_current_session() -> Self {
            let user = std::env::var("USERNAME").ok();
            Self {
                name: pipe_name(win_ffi::current_session_id(), user.as_deref()),
            }
        }
    }

    impl Transport for PipeTransport {
        type Listener = PipeListener;

        fn connect(&self) -> Connect<PipeConn> {
            match OpenOptions::new().read(true).write(true).open(&self.name) {
                Ok(file) => {
                    // Windows only lets the foreground process (this launch,
                    // just started by Explorer or a shell) bring another
                    // process's window to the front, so pass the right on.
                    if let Ok(pid) = win_ffi::pipe_server_process_id(&file) {
                        win_ffi::allow_set_foreground_window(pid);
                    }
                    Connect::Connected(PipeConn(file))
                }
                Err(err) if is_os_error(&err, ERROR_PIPE_BUSY) => Connect::Busy,
                Err(err) if err.kind() == io::ErrorKind::NotFound => Connect::NoServer,
                Err(_) => Connect::Failed,
            }
        }

        fn listen_first(&self) -> io::Result<Option<PipeListener>> {
            match win_ffi::create_pipe_instance(&self.name, true) {
                Ok(current) => Ok(Some(PipeListener {
                    name: self.name.clone(),
                    current,
                })),
                // The pipe exists: another instance (or user) owns it.
                Err(err)
                    if is_os_error(&err, ERROR_ACCESS_DENIED)
                        || is_os_error(&err, ERROR_PIPE_BUSY) =>
                {
                    Ok(None)
                }
                Err(err) => Err(err),
            }
        }
    }

    pub(crate) struct PipeListener {
        name: String,
        /// The instance waiting for the next client.
        current: File,
    }

    impl Listener for PipeListener {
        type Conn = PipeConn;

        fn accept(&mut self) -> io::Result<PipeConn> {
            loop {
                let connected = win_ffi::wait_for_client(&self.current);
                // Create the next instance before handing this one out so
                // the pipe name never disappears (which would let a new
                // launch become a second primary).
                let next = win_ffi::create_pipe_instance(&self.name, false)?;
                let conn = std::mem::replace(&mut self.current, next);
                match connected {
                    Ok(()) => return Ok(PipeConn(conn)),
                    // The client left before we got to it.
                    Err(err) if is_os_error(&err, ERROR_NO_DATA) => {}
                    Err(err) => return Err(err),
                }
            }
        }
    }

    /// One end of a pipe connection.
    pub(crate) struct PipeConn(File);

    impl Read for PipeConn {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl Write for PipeConn {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.write(buf)
        }

        /// Wait until the peer has read everything (`FlushFileBuffers`), so
        /// closing right after cannot drop the ack.
        fn flush(&mut self) -> io::Result<()> {
            self.0.sync_all()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        collections::VecDeque,
        io::{PipeReader, PipeWriter},
    };

    use super::*;

    /// An absolute path on the host OS.
    fn abs(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }

    const FAST: Timing = Timing {
        connect: Duration::from_millis(200),
        exchange: Duration::from_millis(200),
        retry: Duration::from_millis(5),
    };

    /// One end of an in-memory duplex connection.
    struct End {
        reader: PipeReader,
        writer: PipeWriter,
    }

    impl Read for End {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.reader.read(buf)
        }
    }

    impl Write for End {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writer.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.writer.flush()
        }
    }

    fn duplex() -> (End, End) {
        let pipes = io::pipe().and_then(|a| io::pipe().map(|b| (a, b)));
        assert!(pipes.is_ok(), "pipe: {pipes:?}");
        let Ok(((a_reader, b_writer), (b_reader, a_writer))) = pipes else {
            unreachable!()
        };
        (
            End {
                reader: a_reader,
                writer: a_writer,
            },
            End {
                reader: b_reader,
                writer: b_writer,
            },
        )
    }

    /// Listener fed with server ends through a channel; ends when it closes.
    struct FakeListener(mpsc::Receiver<End>);

    impl Listener for FakeListener {
        type Conn = End;

        fn accept(&mut self) -> io::Result<End> {
            self.0
                .recv()
                .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    /// Transport whose answers are scripted; records frames sent to it.
    #[derive(Default)]
    struct FakeTransport {
        connects: RefCell<VecDeque<Connect<End>>>,
        listens: RefCell<VecDeque<io::Result<Option<FakeListener>>>>,
    }

    impl Transport for FakeTransport {
        type Listener = FakeListener;

        fn connect(&self) -> Connect<End> {
            self.connects
                .borrow_mut()
                .pop_front()
                .unwrap_or(Connect::NoServer)
        }

        fn listen_first(&self) -> io::Result<Option<FakeListener>> {
            self.listens.borrow_mut().pop_front().unwrap_or(Ok(None))
        }
    }

    fn fake_listener() -> FakeListener {
        FakeListener(mpsc::channel().1)
    }

    /// A connection to a fake primary that reads one request and answers
    /// with `ack`; returns the client end and the received request.
    fn primary_answering(ack: u8) -> (End, thread::JoinHandle<io::Result<OpenRequest>>) {
        let (client, mut server) = duplex();
        let handle = thread::spawn(move || {
            let request = OpenRequest::read_from(&mut server)?;
            server.write_all(&[ack])?;
            Ok(request)
        });
        (client, handle)
    }

    fn request(names: &[&str]) -> OpenRequest {
        OpenRequest {
            paths: names.iter().map(|name| abs(name)).collect(),
        }
    }

    fn decode(frame: &[u8]) -> io::Result<OpenRequest> {
        OpenRequest::read_from(&mut io::Cursor::new(frame))
    }

    fn frame_with_payload(payload: &[u8]) -> Vec<u8> {
        let mut frame = MAGIC.to_vec();
        frame.extend_from_slice(&u32::try_from(payload.len()).unwrap_or(0).to_le_bytes());
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn request_round_trips() {
        for request in [
            OpenRequest::default(),
            request(&["notes.md"]),
            request(&["a b.md", "ünï cødé.markdown", "x.md"]),
        ] {
            let frame = request.encode();
            assert!(frame.is_some(), "encodable: {request:?}");
            let decoded = decode(&frame.unwrap_or_default());
            assert!(
                matches!(&decoded, Ok(decoded) if *decoded == request),
                "{decoded:?}"
            );
        }
    }

    #[test]
    fn empty_request_means_new_tab_and_has_empty_payload() {
        let frame = OpenRequest::default().encode().unwrap_or_default();
        assert_eq!(frame, frame_with_payload(b""));
    }

    #[test]
    fn encode_rejects_relative_paths() {
        let request = OpenRequest {
            paths: vec![PathBuf::from("relative.md")],
        };
        assert!(request.encode().is_none());
    }

    #[test]
    fn decode_rejects_malformed_frames() {
        let good = request(&["a.md"]).encode().unwrap_or_default();
        let mut bad_magic = good.clone();
        bad_magic[0] ^= 0xFF;
        let mut too_large = MAGIC.to_vec();
        too_large.extend_from_slice(&u32::MAX.to_le_bytes());
        let relative = frame_with_payload(b"relative.md\0");
        let unterminated = frame_with_payload(b"/a.md");
        let empty_path = frame_with_payload(b"\0");
        let not_utf8 = frame_with_payload(b"/\xFF.md\0");
        for frame in [
            bad_magic.as_slice(),
            too_large.as_slice(),
            &good[..good.len() - 1],
            &good[..5],
            relative.as_slice(),
            unterminated.as_slice(),
            empty_path.as_slice(),
            not_utf8.as_slice(),
        ] {
            assert!(decode(frame).is_err(), "accepted {frame:?}");
        }
    }

    #[test]
    fn from_args_resolves_relative_paths_against_the_sender_cwd() {
        let cwd = abs("work");
        let absolute = abs("elsewhere.md");
        let request = OpenRequest::from_args(
            [
                PathBuf::from("notes.md"),
                PathBuf::from("sub").join("..").join("x.md"),
                absolute.clone(),
            ],
            &cwd,
        );
        assert_eq!(
            request.paths,
            vec![
                cwd.join("notes.md"),
                cwd.join("sub").join("..").join("x.md"),
                absolute
            ]
        );
        assert!(request.encode().is_some());
    }

    #[test]
    fn pipe_name_includes_session_and_sanitized_user() {
        // Named after the app id.
        let pipe = format!(r"\\.\pipe\{}", crate::app_identity::APP_ID);
        assert_eq!(
            pipe_name(Some(2), Some("alice")),
            format!("{pipe}-s2-alice")
        );
        assert_eq!(pipe_name(None, None), pipe);
        assert_eq!(pipe_name(Some(0), Some("")), format!("{pipe}-s0"));
        assert_eq!(
            pipe_name(Some(1), Some(r"DOMAIN\Jo é/x")),
            format!("{pipe}-s1-DOMAIN_Jo_é_x")
        );
        let long = pipe_name(Some(1), Some(&"u".repeat(500)));
        assert_eq!(long.len(), format!("{pipe}-s1-").len() + 64);
        // Only the `\\.\pipe\` prefix may contain backslashes.
        assert!(!long[r"\\.\pipe\".len()..].contains('\\'));
    }

    #[test]
    fn elect_forwards_to_a_running_primary() {
        let (client, primary) = primary_answering(ACK_OK);
        let transport = FakeTransport::default();
        transport
            .connects
            .borrow_mut()
            .push_back(Connect::Connected(client));
        let sent = request(&["a.md", "b.md"]);
        let election = elect(&transport, Some(&sent), FAST);
        assert!(matches!(election, Election::Forwarded));
        let received = primary.join().ok().and_then(Result::ok);
        assert_eq!(received, Some(sent));
    }

    #[test]
    fn elect_becomes_primary_when_nobody_listens() {
        let transport = FakeTransport::default();
        transport
            .listens
            .borrow_mut()
            .push_back(Ok(Some(fake_listener())));
        let election = elect(&transport, Some(&request(&["a.md"])), FAST);
        assert!(matches!(election, Election::Primary(_)));
    }

    #[test]
    fn elect_forwards_after_losing_the_race_to_become_primary() {
        // Both launches find no server; the other one creates the pipe first.
        let (client, primary) = primary_answering(ACK_OK);
        let transport = FakeTransport::default();
        transport.connects.borrow_mut().extend([
            Connect::NoServer,
            Connect::Busy,
            Connect::Connected(client),
        ]);
        transport.listens.borrow_mut().push_back(Ok(None));
        let election = elect(&transport, Some(&OpenRequest::default()), FAST);
        assert!(matches!(election, Election::Forwarded));
        let received = primary.join().ok().and_then(Result::ok);
        assert_eq!(received, Some(OpenRequest::default()));
    }

    #[test]
    fn elect_gives_up_on_a_server_that_stays_busy() {
        let transport = FakeTransport::default();
        transport
            .connects
            .borrow_mut()
            .extend(std::iter::repeat_with(|| Connect::Busy).take(10_000));
        let started = Instant::now();
        let election = elect(&transport, Some(&request(&["a.md"])), FAST);
        assert!(matches!(election, Election::Standalone));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn elect_runs_standalone_when_the_primary_hangs() {
        // Connected, but the primary never answers: the launch must not hang.
        let (client, _server) = duplex();
        let transport = FakeTransport::default();
        transport
            .connects
            .borrow_mut()
            .push_back(Connect::Connected(client));
        let started = Instant::now();
        let election = elect(&transport, Some(&request(&["a.md"])), FAST);
        assert!(matches!(election, Election::Standalone));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn elect_runs_standalone_when_the_request_is_rejected_or_unusable() {
        let (client, primary) = primary_answering(ACK_ERR);
        let transport = FakeTransport::default();
        transport
            .connects
            .borrow_mut()
            .push_back(Connect::Connected(client));
        let election = elect(&transport, Some(&request(&["a.md"])), FAST);
        assert!(matches!(election, Election::Standalone));
        let _ = primary.join();

        let transport = FakeTransport::default();
        transport.connects.borrow_mut().push_back(Connect::Failed);
        let election = elect(&transport, Some(&request(&["a.md"])), FAST);
        assert!(matches!(election, Election::Standalone));
    }

    #[test]
    fn new_window_never_forwards() {
        // `--new-window` (no request): becomes primary if possible …
        let transport = FakeTransport::default();
        let (client, _server) = duplex();
        transport
            .connects
            .borrow_mut()
            .push_back(Connect::Connected(client));
        transport
            .listens
            .borrow_mut()
            .push_back(Ok(Some(fake_listener())));
        assert!(matches!(
            elect(&transport, None, FAST),
            Election::Primary(_)
        ));
        assert_eq!(transport.connects.borrow().len(), 1, "never connected");

        // … otherwise runs on its own, without waiting.
        let transport = FakeTransport::default();
        assert!(matches!(
            elect(&transport, None, FAST),
            Election::Standalone
        ));
    }

    #[test]
    fn unsendable_request_falls_back_to_new_window_behaviour() {
        let transport = FakeTransport::default();
        let (client, _server) = duplex();
        transport
            .connects
            .borrow_mut()
            .push_back(Connect::Connected(client));
        let relative = OpenRequest {
            paths: vec![PathBuf::from("relative.md")],
        };
        assert!(matches!(
            elect(&transport, Some(&relative), FAST),
            Election::Standalone
        ));
        assert_eq!(transport.connects.borrow().len(), 1, "never connected");
    }

    #[test]
    fn server_delivers_requests_and_survives_bad_clients() {
        let (conns, accepted) = mpsc::channel();
        let inbox = spawn_server(FakeListener(accepted));
        assert!(inbox.is_ok());
        let Ok(inbox) = inbox else { unreachable!() };
        let ctx = egui::Context::default();
        inbox.attach_ui(ctx, 0);

        // A client that sends garbage gets a rejection …
        let (mut client, server) = duplex();
        assert!(conns.send(server).is_ok());
        assert!(client.write_all(b"garbage-garbage!").is_ok());
        let mut ack = [0];
        assert!(client.read_exact(&mut ack).is_ok());
        assert_eq!(ack[0], ACK_ERR);

        // … a silent client blocks nobody …
        let (_silent, server) = duplex();
        assert!(conns.send(server).is_ok());

        // … and good requests arrive in order.
        for sent in [request(&["one.md"]), OpenRequest::default()] {
            let (client, server) = duplex();
            assert!(conns.send(server).is_ok());
            let frame = sent.encode().unwrap_or_default();
            assert!(exchange(client, frame, Duration::from_secs(5)).is_ok());
            let received = inbox.requests.recv_timeout(Duration::from_secs(5));
            assert_eq!(received.ok(), Some(sent));
        }
    }

    #[test]
    fn requests_before_the_window_exists_wait_in_the_inbox() {
        let (conns, accepted) = mpsc::channel();
        let Ok(inbox) = spawn_server(FakeListener(accepted)) else {
            unreachable!("spawn")
        };
        let (client, server) = duplex();
        assert!(conns.send(server).is_ok());
        let sent = request(&["early.md"]);
        let frame = sent.encode().unwrap_or_default();
        assert!(exchange(client, frame, Duration::from_secs(5)).is_ok());
        let received = inbox.requests.recv_timeout(Duration::from_secs(5));
        assert_eq!(received.ok(), Some(sent));
    }

    #[test]
    fn server_rejects_requests_once_the_window_is_gone() {
        let (conns, accepted) = mpsc::channel();
        let Ok(inbox) = spawn_server(FakeListener(accepted)) else {
            unreachable!("spawn")
        };
        drop(inbox);
        let (client, server) = duplex();
        assert!(conns.send(server).is_ok());
        let frame = request(&["late.md"]).encode().unwrap_or_default();
        let result = exchange(client, frame, Duration::from_secs(5));
        assert!(result.is_err(), "the sender must open its own window");
    }

    #[test]
    fn exchange_reports_a_closed_connection() {
        let (client, server) = duplex();
        drop(server);
        let result = exchange(client, b"x".to_vec(), Duration::from_secs(5));
        assert!(result.is_err());
    }
}
