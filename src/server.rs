use crate::procfs::ProcfsScanner;
use crate::protocol::{AckFrame, ErrorCode, ErrorFrame, Request, error_message, parse_request};
use crate::state::{ActivityError, NameError, StateStore, Subscription, encode_frame};
use crate::{Clock, SystemClock};
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

const MAX_REQUEST_BYTES: usize = 65_536;
const SOCKET_SEND_BUFFER_BYTES: libc::c_int = 65_536;
const SCAN_INTERVAL: Duration = Duration::from_millis(250);
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigterm(_signal: libc::c_int) {
    STOP_REQUESTED.store(true, Ordering::SeqCst);
}

pub fn socket_path() -> Result<PathBuf, String> {
    let runtime =
        std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| "XDG_RUNTIME_DIR is unset".to_owned())?;
    Ok(PathBuf::from(runtime).join("agentd.sock"))
}

pub fn run_daemon() -> Result<(), String> {
    let path = socket_path()?;
    let scanner = ProcfsScanner::system();
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    run_daemon_at(path, scanner, clock).map_err(|error| error.to_string())
}

pub fn run_daemon_at(
    path: PathBuf,
    scanner: ProcfsScanner,
    clock: Arc<dyn Clock>,
) -> io::Result<()> {
    STOP_REQUESTED.store(false, Ordering::SeqCst);
    install_sigterm_handler().map_err(|error| {
        io::Error::new(error.kind(), format!("install SIGTERM handler: {error}"))
    })?;
    let store = Arc::new(StateStore::new().map_err(|error| {
        io::Error::new(error.kind(), format!("create daemon instance ID: {error}"))
    })?);
    let initial = scanner.scan(None, clock.as_ref());
    store.commit_scan(initial);
    let listener = bind_socket(&path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("bind socket {}: {error}", path.display()),
        )
    })?;
    listener.set_nonblocking(true)?;
    let bound = fs::symlink_metadata(&path)?;

    let scanner_store = store.clone();
    let scanner_clock = clock.clone();
    let scanner_thread = thread::spawn(move || {
        while !STOP_REQUESTED.load(Ordering::SeqCst) {
            thread::sleep(SCAN_INTERVAL);
            if STOP_REQUESTED.load(Ordering::SeqCst) {
                break;
            }
            let previous = scanner_store.current_snapshot();
            let proposal = scanner.scan(previous.as_deref(), scanner_clock.as_ref());
            scanner_store.commit_scan(proposal);
        }
    });

    while !STOP_REQUESTED.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let store = store.clone();
                let clock = clock.clone();
                thread::spawn(move || {
                    let _ = serve_connection(stream, store, clock);
                });
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                STOP_REQUESTED.store(true, Ordering::SeqCst);
                store.close_subscribers();
                drop(listener);
                let _ = scanner_thread.join();
                remove_if_same_socket(&path, &bound)?;
                return Err(error);
            }
        }
    }

    store.close_subscribers();
    drop(listener);
    scanner_thread
        .join()
        .map_err(|_| io::Error::other("procfs scanner thread panicked"))?;
    remove_if_same_socket(&path, &bound)
}

fn install_sigterm_handler() -> io::Result<()> {
    let handler = handle_sigterm as *const () as libc::sighandler_t;
    let result = unsafe { libc::signal(libc::SIGTERM, handler) };
    if result == libc::SIG_ERR {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn bind_socket(path: &Path) -> io::Result<UnixListener> {
    loop {
        match fs::symlink_metadata(path) {
            Ok(metadata) => inspect_existing_socket(path, &metadata)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        match UnixListener::bind(path) {
            Ok(listener) => {
                fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
                return Ok(listener);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::AddrInUse | io::ErrorKind::AlreadyExists
                ) => {}
            Err(error) => return Err(error),
        }
    }
}

fn inspect_existing_socket(path: &Path, first: &fs::Metadata) -> io::Result<()> {
    if !first.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("refusing to remove non-socket path {}", path.display()),
        ));
    }
    let effective_uid = unsafe { libc::geteuid() as u32 };
    if first.uid() != effective_uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing to remove socket not owned by local user: {}",
                path.display()
            ),
        ));
    }
    match UnixStream::connect(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("live listener already owns {}", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
            let second = fs::symlink_metadata(path)?;
            if !second.file_type().is_socket()
                || second.uid() != effective_uid
                || second.dev() != first.dev()
                || second.ino() != first.ino()
            {
                return Err(io::Error::other(format!(
                    "socket changed during stale-path check: {}",
                    path.display()
                )));
            }
            fs::remove_file(path)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io::Error::new(
            error.kind(),
            format!(
                "cannot establish stale socket state for {}: {error}",
                path.display()
            ),
        )),
    }
}

fn remove_if_same_socket(path: &Path, bound: &fs::Metadata) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(current)
            if current.file_type().is_socket()
                && current.dev() == bound.dev()
                && current.ino() == bound.ino() =>
        {
            fs::remove_file(path)
        }
        Ok(_) => Err(io::Error::other(format!(
            "socket path changed before shutdown cleanup: {}",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn serve_connection(
    mut stream: UnixStream,
    store: Arc<StateStore>,
    clock: Arc<dyn Clock>,
) -> io::Result<()> {
    configure_send_buffer(&stream)?;
    let bytes = match read_request(&mut stream) {
        Ok(bytes) => bytes,
        Err(code) => return write_error(&mut stream, code),
    };
    let request = match parse_request(&bytes) {
        Ok(request) => request,
        Err(code) => return write_error(&mut stream, code),
    };
    match request {
        Request::Snapshot => stream.write_all(store.snapshot_frame().as_slice()),
        Request::Subscribe => serve_subscription(&mut stream, store.subscribe()),
        Request::Activity { agent, state } => {
            match store.apply_activity(agent, state, clock.as_ref()) {
                Ok(ack) => stream.write_all(&encode_frame(&AckFrame {
                    frame_type: "ack",
                    instance_id: &ack.instance_id,
                    revision: ack.revision,
                })),
                Err(ActivityError::UnknownAgent) => {
                    write_error(&mut stream, ErrorCode::UnknownAgent)
                }
            }
        }
        Request::Name { agent, name } => match store.apply_name(agent, name) {
            Ok(ack) => stream.write_all(&encode_frame(&AckFrame {
                frame_type: "ack",
                instance_id: &ack.instance_id,
                revision: ack.revision,
            })),
            Err(NameError::UnknownAgent) => write_error(&mut stream, ErrorCode::UnknownAgent),
            Err(NameError::StoreUnavailable) => {
                write_error(&mut stream, ErrorCode::NameStoreUnavailable)
            }
        },
    }
}

fn serve_subscription(stream: &mut UnixStream, subscription: Subscription) -> io::Result<()> {
    let Subscription { initial, slot } = subscription;
    let initial_result = write_owned_frame(stream, initial);
    if initial_result.is_err() {
        slot.close();
        return Ok(());
    }
    while let Some(frame) = slot.take() {
        if stream.write_all(frame.as_slice()).is_err() {
            slot.close();
            break;
        }
    }
    Ok(())
}

fn write_owned_frame<W: Write>(writer: &mut W, frame: Arc<Vec<u8>>) -> io::Result<()> {
    writer.write_all(frame.as_slice())
}

fn read_request(stream: &mut UnixStream) -> Result<Vec<u8>, ErrorCode> {
    let mut object = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 4096];
    loop {
        let count = stream
            .read(&mut chunk)
            .map_err(|_| ErrorCode::MalformedRequest)?;
        if count == 0 {
            return Err(ErrorCode::MalformedRequest);
        }
        for byte in &chunk[..count] {
            if *byte == b'\n' {
                return Ok(object);
            }
            if object.len() == MAX_REQUEST_BYTES {
                return Err(ErrorCode::RequestTooLarge);
            }
            object.push(*byte);
        }
    }
}

fn write_error(stream: &mut UnixStream, code: ErrorCode) -> io::Result<()> {
    stream.write_all(&encode_frame(&ErrorFrame {
        frame_type: "error",
        code,
        message: error_message(code),
    }))
}

fn configure_send_buffer(stream: &UnixStream) -> io::Result<()> {
    let value = SOCKET_SEND_BUFFER_BYTES;
    let result = unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            (&value as *const libc::c_int).cast(),
            std::mem::size_of_val(&value) as libc::socklen_t,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Weak;

    #[test]
    fn owned_initial_frame_is_released_when_its_write_completes() {
        let retained_current = Arc::new(b"initial\n".to_vec());
        let initial_lifetime: Weak<Vec<u8>> = Arc::downgrade(&retained_current);
        let subscriber_initial = retained_current.clone();
        assert_eq!(initial_lifetime.strong_count(), 2);

        let mut written = Vec::new();
        write_owned_frame(&mut written, subscriber_initial).unwrap();
        assert_eq!(written, b"initial\n");
        assert_eq!(
            initial_lifetime.strong_count(),
            1,
            "subscriber retained the already-sent initial frame"
        );
    }
}
