//! Listening, serving and the shutdown order.
//!
//! On the stop signal, [`serve`] sets one deadline, `SHUTDOWN_GRACE_PERIOD`
//! away, and everything after shares it. It cancels the supervisor's token,
//! the one stop every background loop and plugin process hears, and ends
//! open event streams. Then it drains HTTP connections while the supervisor
//! stops its loops, both until the deadline. A long stream held open by a
//! client therefore never keeps scans or downloads running. Whatever is
//! still running at the deadline is aborted with a warning. The provider
//! cache file's writer then commits what it holds and stops, the database
//! closes last with what is left, and the caller gets the rest for its own
//! runtime and the data lock to hold until the process exits.

use std::{
    future::{Future, IntoFuture as _},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use axum::Router;
use thiserror::Error;
use tokio::{net::TcpListener, time::Instant};

use droppedneedle_kernel::{
    config::BindHost, db::DbRuntime, db::cache::CacheDb, db::datalock::DataLock, events::EventHub,
    jobs::Supervisor,
};

/// Listen backlog, the usual server default.
const BACKLOG: i32 = 1024;

/// Past the deadline, the least time the database still gets, the same the
/// loops' last rows get.
const LAST_WRITES_FLOOR: Duration = Duration::from_secs(1);

/// Why serving stopped with an error.
#[derive(Debug, Error)]
pub enum ServeError {
    /// The listener could not bind or accept.
    #[error("cannot listen on {address}: {source}")]
    Bind {
        /// Address tried.
        address: SocketAddr,
        /// Underlying failure.
        source: std::io::Error,
    },
    /// The HTTP server stopped on an I/O error.
    #[error("server fault: {0}")]
    Io(#[from] std::io::Error),
    /// The HTTP server task panicked.
    #[error("server task failed: {0}")]
    Task(String),
}

/// Bind the listener. `auto` tries one dual-stack `[::]` socket with
/// `IPV6_V6ONLY` off, so IPv4 clients arrive as mapped addresses, and
/// falls back to `0.0.0.0` where the host has IPv6 off. A fixed address
/// binds exactly as given.
pub async fn bind(host: BindHost, port: u16) -> Result<TcpListener, ServeError> {
    let fixed = match host {
        BindHost::Fixed(ip) => ip,
        BindHost::Auto => {
            let dual = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port);
            match dual_stack_listener(dual) {
                Ok(listener) => return Ok(listener),
                Err(error) => {
                    tracing::info!(%error, "IPv6 unavailable; listening on IPv4 only");
                    IpAddr::V4(Ipv4Addr::UNSPECIFIED)
                }
            }
        }
    };
    let address = SocketAddr::new(fixed, port);
    TcpListener::bind(address)
        .await
        .map_err(|source| ServeError::Bind { address, source })
}

fn dual_stack_listener(address: SocketAddr) -> std::io::Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};

    let socket = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_only_v6(false)?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&address.into())?;
    socket.listen(BACKLOG)?;
    TcpListener::from_std(socket.into())
}

/// Background work started at boot, stopped by [`serve`].
pub struct Background {
    grace: Duration,
    supervisor: Supervisor,
    events: EventHub,
    runtime: DbRuntime,
    cache_db: CacheDb,
    data_lock: DataLock,
}

impl Background {
    /// The supervisor every loop runs on, whose token is the one stop; the
    /// hub whose streams end at shutdown; the database, closed last; and
    /// the provider cache file, stopped after the loops; and the data lock,
    /// handed back to the caller at the end.
    pub fn new(
        grace: Duration,
        supervisor: Supervisor,
        events: EventHub,
        runtime: DbRuntime,
        cache_db: CacheDb,
        data_lock: DataLock,
    ) -> Self {
        Self {
            grace,
            supervisor,
            events,
            runtime,
            cache_db,
            data_lock,
        }
    }
}

/// How serving ended.
#[derive(Debug)]
pub struct Stopped {
    /// Whether the HTTP server stopped cleanly.
    pub outcome: Result<(), ServeError>,
    /// What is left of the shutdown budget, for the threads still running.
    pub left: Duration,
    /// The shared data lock. A write or file move the shutdown gave up on
    /// may still be running, so the caller holds this until the process
    /// exits.
    pub data_lock: DataLock,
}

/// Serve `router` on `listener` until `signal` resolves (or the server
/// fails), then shut down in order under one budget of the configured
/// grace. Peers reach handlers as `ConnectInfo<SocketAddr>`.
pub async fn serve(
    listener: TcpListener,
    router: Router,
    background: Background,
    signal: impl Future<Output = ()> + Send,
) -> Stopped {
    let Background {
        grace,
        supervisor,
        events,
        runtime,
        cache_db,
        data_lock,
    } = background;
    let stop = supervisor.token().clone();
    let server = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(stop.clone().cancelled_owned());
    let mut server = tokio::spawn(server.into_future());
    let ended_early = tokio::select! {
        () = signal => None,
        outcome = &mut server => Some(outcome),
    };
    // One deadline for everything below. Loops, plugin processes and the
    // HTTP drain hear the stop together. Event streams never end on their
    // own, so they close here, before draining starts.
    let deadline = Instant::now() + grace;
    stop.cancel();
    events.close();
    let drain = async {
        if let Some(outcome) = ended_early {
            return outcome;
        }
        match tokio::time::timeout_at(deadline, &mut server).await {
            Ok(outcome) => outcome,
            Err(_) => {
                tracing::warn!(
                    ?grace,
                    "connections still open at the shutdown bound; closing"
                );
                server.abort();
                Ok(Ok(()))
            }
        }
    };
    let (served, ()) = tokio::join!(
        drain,
        supervisor.shutdown(deadline.saturating_duration_since(Instant::now()))
    );
    // The database gets what is left, with the same floor the loops' last
    // rows get, so a write queued at the deadline still has a moment.
    let last = deadline + LAST_WRITES_FLOOR;
    // The provider cache's writer commits what it holds, then stops and
    // lets go of the file's lock, so a server started next can open it.
    cache_db
        .shutdown_within(last.saturating_duration_since(Instant::now()))
        .await;
    runtime
        .shutdown_within(last.saturating_duration_since(Instant::now()))
        .await;
    tracing::info!("shutdown complete");
    Stopped {
        outcome: match served {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(ServeError::Io(error)),
            Err(error) => Err(ServeError::Task(error.to_string())),
        },
        left: last.saturating_duration_since(Instant::now()),
        data_lock,
    }
}
