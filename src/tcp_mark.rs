//! A TCP provider that stamps a firewall mark on every socket Arti opens.
//!
//! Hosts that embed arti-c to carry a transparent VPN install firewall and
//! policy-routing rules that divert every outbound packet into a local
//! transparent proxy. Arti's own connections to Tor relays and directory
//! authorities must not be diverted: they would be fed straight back into the
//! tunnel Arti is supposed to be carrying, recursing until the process runs
//! out of file descriptors. Those hosts exempt the proxy core's own traffic by
//! marking its sockets (`SO_MARK`) and having the rules pass marked packets
//! through untouched.
//!
//! Arti creates its sockets deep inside `arti-client`, so the only place a
//! mark can be applied is the runtime's TCP provider. `tor-rtcompat` allows a
//! provider to be substituted into an otherwise stock runtime
//! ([`RuntimeSubstExt::with_tcp_provider`]), which is what this module does:
//! [`MarkedTcpProvider`] builds each socket itself, applies the mark, and then
//! hands it to tokio to connect.
//!
//! A mark of `0` means "do not mark", which is exactly the behaviour of a
//! stock runtime and is what callers that do not need an exemption pass.

use std::io::{Error, ErrorKind, Result as IoResult};
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use async_trait::async_trait;
use futures::io::{AsyncRead, AsyncWrite};
use futures::stream::Stream;
use socket2::{Domain, Socket, Type};
use tokio::net::{TcpSocket, TcpStream};
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt};

use tor_rtcompat::{
    NetStreamListener, NetStreamProvider, PreferredRuntime, StreamOps, TcpConnectOptions,
    TcpListenOptions,
};

/// A TCP provider that applies `SO_MARK` to every socket it opens.
///
/// Every other part of the runtime is the stock [`PreferredRuntime`], so this
/// is only ever used through `PreferredRuntime::with_tcp_provider()`.
#[derive(Clone)]
pub struct MarkedTcpProvider {
    /// The stock runtime, used for the listener path this provider does not
    /// implement and for the tokio handle the sockets are connected on.
    inner: PreferredRuntime,
    /// Value for `SO_MARK`. Zero disables marking.
    mark: u32,
}

impl MarkedTcpProvider {
    /// Wrap `inner`, marking every connected socket with `mark`.
    pub fn new(inner: PreferredRuntime, mark: u32) -> Self {
        MarkedTcpProvider { inner, mark }
    }
}

impl std::fmt::Debug for MarkedTcpProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarkedTcpProvider")
            .field("mark", &self.mark)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl NetStreamProvider<SocketAddr> for MarkedTcpProvider {
    type Stream = MarkedTcpStream;
    type Listener = MarkedTcpListener;
    type ConnectOptions = TcpConnectOptions;
    type ListenOptions = TcpListenOptions;

    /// Open a socket, apply the mark to it, and connect it through tokio.
    ///
    /// This mirrors what `tor-rtcompat`'s own tokio provider does, with the
    /// mark applied between `socket(2)` and `connect(2)` -- the only window in
    /// which `SO_MARK` has any effect on the connection.
    ///
    /// `options` is deliberately unused: its only fields are socket buffer
    /// sizes, which are private to `tor-rtcompat`, and Arti always passes the
    /// default (both unset) in any case.
    async fn connect(
        &self,
        addr: &SocketAddr,
        _options: &Self::ConnectOptions,
    ) -> IoResult<Self::Stream> {
        let domain = match addr {
            SocketAddr::V4(_) => Domain::IPV4,
            SocketAddr::V6(_) => Domain::IPV6,
        };

        let socket = Socket::new(domain, Type::STREAM, None)?;

        if self.mark != 0 {
            mark_socket(&socket, self.mark)?;
        }

        // tokio requires the socket to be non-blocking before it takes it
        // over, and `TcpSocket::from_std_stream` expects an *unconnected*
        // socket, which is why the mark has to be applied here.
        socket.set_nonblocking(true)?;

        let socket = TcpSocket::from_std_stream(std::net::TcpStream::from(socket));
        let stream = socket.connect(*addr).await?;

        Ok(MarkedTcpStream {
            inner: stream.compat(),
        })
    }

    /// Unsupported: Arti is used here as a client only, and a marked listener
    /// would need a listener type able to produce [`MarkedTcpStream`]s.
    async fn listen(
        &self,
        _addr: &SocketAddr,
        _options: &Self::ListenOptions,
    ) -> IoResult<Self::Listener> {
        Err(Error::new(
            ErrorKind::Unsupported,
            "the marked TCP provider is client-only",
        ))
    }
}

/// Apply `SO_MARK` to `socket`, or report that the platform cannot.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn mark_socket(socket: &Socket, mark: u32) -> IoResult<()> {
    socket.set_mark(mark)
}

/// `SO_MARK` is Linux-specific. Reporting an error rather than silently
/// connecting unmarked keeps a caller that asked for an exemption from
/// believing it got one.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn mark_socket(_socket: &Socket, _mark: u32) -> IoResult<()> {
    Err(Error::new(
        ErrorKind::Unsupported,
        "SO_MARK is not available on this platform",
    ))
}

/// A connected TCP stream, in the form `tor-rtcompat` expects.
///
/// `tor-rtcompat`'s own stream type for the tokio runtime is private, so a
/// provider outside that crate has to supply its own.
pub struct MarkedTcpStream {
    inner: Compat<TcpStream>,
}

impl AsyncRead for MarkedTcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<IoResult<usize>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for MarkedTcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<IoResult<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<IoResult<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<IoResult<()>> {
        Pin::new(&mut self.inner).poll_close(cx)
    }
}

/// Every method of [`StreamOps`] has a default implementation, so this only
/// has to opt in. `TCP_NOTSENT_LOWAT` and the handle-based operations are
/// therefore reported as unsupported; Arti treats both as optional.
impl StreamOps for MarkedTcpStream {}

/// Placeholder listener required by [`NetStreamProvider`]'s associated-type
/// bounds.
///
/// This type is never constructed: [`MarkedTcpProvider::listen`] always fails,
/// so neither it nor its incoming stream can be reached.
pub struct MarkedTcpListener {
    _never: std::convert::Infallible,
}

/// Placeholder incoming-connection stream for [`MarkedTcpListener`]. Never
/// constructed; see that type.
pub struct MarkedIncoming {
    _never: std::convert::Infallible,
}

impl Stream for MarkedIncoming {
    type Item = IoResult<(MarkedTcpStream, SocketAddr)>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self._never {}
    }
}

impl NetStreamListener<SocketAddr> for MarkedTcpListener {
    type Stream = MarkedTcpStream;
    type Incoming = MarkedIncoming;

    fn incoming(self) -> Self::Incoming {
        match self._never {}
    }

    fn local_addr(&self) -> IoResult<SocketAddr> {
        match self._never {}
    }
}

// `inner` is retained so the stock runtime outlives the sockets connected
// through it.
#[allow(dead_code)]
impl MarkedTcpProvider {
    fn runtime(&self) -> &PreferredRuntime {
        &self.inner
    }
}
