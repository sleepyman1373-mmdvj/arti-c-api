//! C ABI wrapper around Arti (arti-client), exposing a minimal, stable API.
//!
//! Nothing but C types and opaque handles cross the ABI boundary. Arti runs
//! in-process on its own thread with a multi-threaded tokio runtime, and a
//! local SOCKS5 proxy is served on 127.0.0.1:<port> (see the `socks` module).

mod socks;

use arti_client::config::TorClientConfigBuilder;
use arti_client::{TorClient, TorClientConfig};
use std::ffi::{c_char, CStr, CString};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use tokio::sync::watch;

/// Opaque handle returned by `arti_start` and consumed by every other
/// entry point. Never dereferenced on the C side.
pub struct Arti {
    ready: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    socks_port: AtomicU16,
    shutdown_tx: watch::Sender<bool>,
    join: Option<std::thread::JoinHandle<()>>,
    last_error: Arc<Mutex<Option<CString>>>,
}

/// Error message from a failed `arti_start` (returned to C by
/// `arti_last_error(NULL)`).
static STARTUP_ERROR: Mutex<Option<CString>> = Mutex::new(None);

static VERSION: std::sync::OnceLock<CString> = std::sync::OnceLock::new();

fn set_err(slot: &Mutex<Option<CString>>, msg: String) {
    let cmsg = CString::new(msg)
        .unwrap_or_else(|_| CString::new("error message contained NUL byte").unwrap());
    *slot.lock().expect("error slot poisoned") = Some(cmsg);
}

/// Return version information as a static, NUL-terminated string.
/// The pointer is valid for the lifetime of the process; do not free it.
#[no_mangle]
pub extern "C" fn arti_version() -> *const c_char {
    VERSION
        .get_or_init(|| {
            CString::new(format!("arti-c {}", env!("CARGO_PKG_VERSION"))).expect("no NUL")
        })
        .as_ptr()
}

/// Start the Arti client in-process and begin serving a SOCKS5 proxy on
/// 127.0.0.1:`socks_port`. If `socks_port` is 0, the default port 9050 is
/// used.
///
/// `data_dir` may be NULL, in which case platform-default Arti storage
/// directories are used. Otherwise it names a directory that will hold the
/// persistent Tor state (`state/`) and cache (`cache/`) subdirectories; it
/// is created if missing.
///
/// Returns an opaque handle, or NULL on failure. On failure the reason is
/// available via `arti_last_error(NULL)`. Starting the client does not wait
/// for bootstrap; poll `arti_is_ready()` for that. The returned handle must
/// eventually be released with exactly one call to `arti_stop()`.
#[no_mangle]
pub extern "C" fn arti_start(data_dir: *const c_char, socks_port: u16) -> *mut Arti {
    *STARTUP_ERROR.lock().expect("startup error slot poisoned") = None;

    let dir = if data_dir.is_null() {
        None
    } else {
        let bytes = unsafe { CStr::from_ptr(data_dir) };
        match bytes.to_str() {
            Ok(s) => Some(PathBuf::from(s)),
            Err(_) => {
                set_err(&STARTUP_ERROR, "data_dir is not valid UTF-8".to_string());
                return ptr::null_mut();
            }
        }
    };

    let socks_port = if socks_port == 0 { 9050 } else { socks_port };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (init_tx, init_rx) = mpsc::sync_channel::<Result<u16, String>>(1);
    let ready = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicBool::new(false));
    let last_error: Arc<Mutex<Option<CString>>> = Arc::new(Mutex::new(None));

    let join = std::thread::Builder::new()
        .name("arti-c".to_string())
        .spawn(move || {
            run_thread(
                dir, socks_port, ready, failed, last_error, shutdown_rx, init_tx,
            );
        });

    let join = match join {
        Ok(j) => j,
        Err(e) => {
            set_err(&STARTUP_ERROR, format!("failed to spawn Arti thread: {e}"));
            return ptr::null_mut();
        }
    };

    match init_rx.recv_timeout(std::time::Duration::from_secs(120)) {
        Ok(Ok(port)) => Box::into_raw(Box::new(Arti {
            ready: Arc::new(AtomicBool::new(false)),
            failed: Arc::new(AtomicBool::new(false)),
            socks_port: AtomicU16::new(port),
            shutdown_tx,
            join: Some(join),
            last_error: Arc::new(Mutex::new(None)),
        })),
        Ok(Err(msg)) => {
            set_err(&STARTUP_ERROR, msg);
            let _ = join.join();
            ptr::null_mut()
        }
        Err(_) => {
            set_err(
                &STARTUP_ERROR,
                "timed out waiting for Arti startup (config load or port bind)".to_string(),
            );
            ptr::null_mut()
        }
    }
}

/// Return 1 if the client has fully bootstrapped onto the Tor network, 0 if
/// it is still connecting, and -1 on error (including a NULL handle or a
/// failed bootstrap; in that case `arti_last_error()` explains why).
#[no_mangle]
pub extern "C" fn arti_is_ready(a: *const Arti) -> i32 {
    if a.is_null() {
        return -1;
    }
    let a = unsafe { &*a };
    if a.failed.load(Ordering::SeqCst) {
        return -1;
    }
    i32::from(a.ready.load(Ordering::SeqCst))
}

/// Return the port the SOCKS5 proxy is actually listening on
/// (127.0.0.1:<port>), or 0 on error / invalid handle.
#[no_mangle]
pub extern "C" fn arti_socks_port(a: *const Arti) -> u16 {
    if a.is_null() {
        return 0;
    }
    unsafe { (&*a).socks_port.load(Ordering::SeqCst) }
}

/// Return the most recent error message for this handle as a
/// NUL-terminated string, or NULL if there is none. With a NULL handle,
/// return the error from the most recent failed `arti_start` call.
///
/// The returned pointer remains valid until the next call to any `arti_*`
/// function on the same handle (or with NULL, the next `arti_start`). It
/// must not be freed.
#[no_mangle]
pub extern "C" fn arti_last_error(a: *const Arti) -> *const c_char {
    if a.is_null() {
        return match &*STARTUP_ERROR.lock().expect("startup error slot poisoned") {
            Some(s) => s.as_ptr(),
            None => ptr::null(),
        };
    }
    let a = unsafe { &*a };
    let slot = a.last_error.lock().expect("error slot poisoned");
    match &*slot {
        Some(s) => s.as_ptr(),
        None => ptr::null(),
    }
}

/// Shut the client down, free the handle, and release all resources.
/// After this call the handle is invalid and must not be used again.
/// Passing NULL is a no-op. The call blocks until shutdown completes.
#[no_mangle]
pub extern "C" fn arti_stop(a: *mut Arti) {
    if a.is_null() {
        return;
    }
    let a = unsafe { Box::from_raw(a) };
    let mut a = a;
    let _ = a.shutdown_tx.send(true);
    if let Some(join) = a.join.take() {
        let _ = join.join();
    }
    // `a` is dropped here, releasing the handle's memory.
}

/// Body of the Arti thread: build a tokio runtime, construct the client,
/// bind the SOCKS listener, report the bound port, then serve until
/// shutdown is requested.
fn run_thread(
    dir: Option<PathBuf>,
    socks_port: u16,
    ready: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    last_error: Arc<Mutex<Option<CString>>>,
    shutdown_rx: watch::Receiver<bool>,
    init_tx: mpsc::SyncSender<Result<u16, String>>,
) {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = init_tx.send(Err(format!("failed to build tokio runtime: {e}")));
            return;
        }
    };

    // Arti logs through `tracing`; install a subscriber writing to stderr.
    // Verbosity is controlled by the RUST_LOG environment variable.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let result = rt.block_on(async_main(
        dir,
        socks_port,
        Arc::clone(&ready),
        Arc::clone(&failed),
        Arc::clone(&last_error),
        shutdown_rx,
        &init_tx,
    ));

    if let Err(msg) = result {
        // Startup failed after init_tx was already consumed; record it so
        // `arti_is_ready`/`arti_last_error` can surface it.
        set_err(&last_error, msg);
        failed.store(true, Ordering::SeqCst);
    }

    // Dropping the runtime aborts the accept loop and drops the client,
    // closing all Tor connections.
    rt.shutdown_timeout(std::time::Duration::from_secs(5));
}

async fn async_main(
    dir: Option<PathBuf>,
    socks_port: u16,
    ready: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    last_error: Arc<Mutex<Option<CString>>>,
    mut shutdown_rx: watch::Receiver<bool>,
    init_tx: &mpsc::SyncSender<Result<u16, String>>,
) -> Result<u16, String> {
    let config = build_config(dir.as_ref())?;

    let client = TorClient::builder()
        .config(config)
        .create_unbootstrapped_async()
        .await
        .map_err(|e| format!("failed to create Arti client: {e}"))?;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", socks_port))
        .await
        .map_err(|e| format!("failed to bind SOCKS listener on 127.0.0.1:{socks_port}: {e}"))?;
    let bound_port = listener
        .local_addr()
        .map_err(|e| format!("failed to read bound SOCKS port: {e}"))?
        .port();

    // Bootstrap in the background; `arti_is_ready` observes the outcome.
    let boot_client = Arc::clone(&client);
    let boot_ready = Arc::clone(&ready);
    let boot_failed = Arc::clone(&failed);
    let boot_err = Arc::clone(&last_error);
    tokio::spawn(async move {
        if let Err(e) = boot_client.bootstrap().await {
            set_err(&boot_err, format!("bootstrap failed: {e}"));
            boot_failed.store(true, Ordering::SeqCst);
        } else {
            boot_ready.store(true, Ordering::SeqCst);
        }
    });

    let accept_task = tokio::spawn(socks::accept_loop(
        listener,
        client,
        shutdown_rx.clone(),
        Arc::clone(&last_error),
    ));

    let _ = init_tx.send(Ok(bound_port));

    // Block until shutdown is requested (sent by `arti_stop`).
    loop {
        if *shutdown_rx.borrow_and_update() {
            break;
        }
        if shutdown_rx.changed().await.is_err() {
            break;
        }
    }
    accept_task.abort();

    Ok(bound_port)
}

fn build_config(dir: Option<&PathBuf>) -> Result<TorClientConfig, String> {
    match dir {
        None => Ok(TorClientConfig::default()),
        Some(d) => {
            let state_dir = d.join("state");
            let cache_dir = d.join("cache");
            TorClientConfigBuilder::from_directories(state_dir, cache_dir)
                .build()
                .map_err(|e| format!("failed to build Arti configuration: {e}"))
        }
    }
}
