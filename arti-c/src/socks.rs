//! Minimal SOCKS5 proxy server backed by an Arti `TorClient`.
//!
//! Mirrors the approach used by the `arti` binary's own proxy module
//! (`crates/arti/src/proxy/socks.rs` upstream): the SOCKS handshake is
//! driven with `tor-socksproto`, then the negotiated connection is relayed
//! through a Tor data stream. Only C-free Rust types appear here; this
//! module is entirely behind the FFI boundary.

use arti_client::{ErrorKind, HasKind as _, IntoTorAddr as _, StreamPrefs, TorClient};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tor_rtcompat::Runtime;
use tor_socksproto::{
    Handshake as _, NextStep as NS, SocksAddr, SocksCmd, SocksProxyHandshake, SocksRequest,
    SocksStatus,
};

/// Accept SOCKS connections on `listener` until `shutdown` is signalled.
pub(super) async fn accept_loop<R: Runtime>(
    listener: TcpListener,
    client: Arc<TorClient<R>>,
    mut shutdown: watch::Receiver<bool>,
    last_error: Arc<std::sync::Mutex<Option<std::ffi::CString>>>,
) {
    loop {
        tokio::select! {
            _ = shutdown.changed() => return,
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _peer)) => {
                        let client = Arc::clone(&client);
                        tokio::spawn(handle_conn(stream, client));
                    }
                    Err(e) => {
                        crate::set_err(
                            &last_error,
                            format!("error accepting SOCKS connection: {e}"),
                        );
                        return;
                    }
                }
            }
        }
    }
}

/// Perform the SOCKS handshake on one connection and relay traffic over Tor.
async fn handle_conn<R: Runtime>(mut stream: TcpStream, client: Arc<TorClient<R>>) {
    let mut handshake = SocksProxyHandshake::new();
    let mut inbuf = tor_socksproto::Buffer::new();

    let request = loop {
        let step = match handshake.step(&mut inbuf) {
            Ok(step) => step,
            Err(_) => return, // peer is not speaking SOCKS
        };
        match step {
            NS::Recv(mut recv) => {
                let n = match stream.read(recv.buf()).await {
                    Ok(0) | Err(_) => return, // EOF or read error mid-handshake
                    Ok(n) => n,
                };
                if recv.note_received(n).is_err() {
                    return;
                }
            }
            NS::Send(data) => {
                if stream.write_all(&data).await.is_err() {
                    return;
                }
            }
            NS::Finished(fin) => match fin.into_output_forbid_pipelining() {
                Ok(req) => break req,
                Err(_) => return,
            },
        }
    };

    let addr = request.addr().to_string();
    let port = request.port();

    let mut prefs = StreamPrefs::new();
    if addr.parse::<Ipv4Addr>().is_ok() {
        prefs.ipv4_only();
    } else if addr.parse::<Ipv6Addr>().is_ok() {
        prefs.ipv6_only();
    }

    match request.command() {
        SocksCmd::CONNECT => {
            let tor_addr = match (addr.as_str(), port).into_tor_addr() {
                Ok(a) => a,
                Err(_) => {
                    reply_error(&mut stream, &request, SocksStatus::ADDRTYPE_NOT_SUPPORTED)
                        .await;
                    return;
                }
            };
            match client.connect_with_prefs(&tor_addr, &prefs).await {
                Ok(mut tor_stream) => {
                    if let Some(reply) = request.reply(SocksStatus::SUCCEEDED, None).ok() {
                        if stream.write_all(&reply).await.is_err() {
                            return;
                        }
                        let _ = stream.flush().await;
                    }
                    let _ = tokio::io::copy_bidirectional(&mut stream, &mut tor_stream).await;
                }
                Err(e) => {
                    reply_error(&mut stream, &request, socks_status(&e.kind())).await;
                }
            }
        }
        SocksCmd::RESOLVE => {
            let resolved = match addr.parse() {
                Ok(ip) => Some(ip),
                Err(_) => client
                    .resolve_with_prefs(&addr, &prefs)
                    .await
                    .ok()
                    .and_then(|addrs| addrs.first().copied()),
            };
            match resolved {
                Some(ip) => {
                    if let Some(reply) = request
                        .reply(SocksStatus::SUCCEEDED, Some(&SocksAddr::Ip(ip)))
                        .ok()
                    {
                        let _ = stream.write_all(&reply).await;
                        let _ = stream.shutdown().await;
                    }
                }
                None => {
                    reply_error(&mut stream, &request, SocksStatus::GENERAL_FAILURE).await;
                }
            }
        }
        SocksCmd::RESOLVE_PTR => {
            reply_error(&mut stream, &request, SocksStatus::COMMAND_NOT_SUPPORTED).await;
        }
        _ => {
            reply_error(&mut stream, &request, SocksStatus::COMMAND_NOT_SUPPORTED).await;
        }
    }
}

fn socks_status(kind: &ErrorKind) -> SocksStatus {
    match kind {
        ErrorKind::ExitPolicyRejected => SocksStatus::NOT_ALLOWED,
        ErrorKind::RemoteConnectionRefused => SocksStatus::CONNECTION_REFUSED,
        ErrorKind::RemoteHostNotFound | ErrorKind::RemoteHostResolutionFailed => {
            SocksStatus::HOST_UNREACHABLE
        }
        _ => SocksStatus::GENERAL_FAILURE,
    }
}

async fn reply_error(stream: &mut TcpStream, request: &SocksRequest, status: SocksStatus) {
    if let Ok(reply) = request.reply(status, None) {
        let _ = stream.write_all(&reply).await;
        let _ = stream.shutdown().await;
    }
}
