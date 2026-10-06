//! Listening, serving and the shutdown order.
//!
//! On the stop signal, [`serve`] first tells every background loop to stop
//! (one watch channel) and ends open event streams, then drains HTTP
//! connections, cancels registry jobs and joins the loops at the same
//! time, each bounded by `SHUTDOWN_GRACE_PERIOD`. A long stream held open
//! by a client therefore never keeps scans or downloads running. Whatever
//! is still running at the bound is aborted with a warning. The database
//! closes last.

use std::{
    future::{Future, IntoFuture as _},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use axum::Router;
use thiserror::Error;
use tokio::{net::TcpListener, sync::watch, task::JoinHandle};

use crate::{
    config::BindHost, db::DbRuntime, events::EventHub, jobs::wiring::JobsSetup,
    tooling::datalock::DataLock,
};

/// Listen backlog, the usual server default.
const BACKLOG: i32 = 1024;

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
    stop: watch::Sender<bool>,
    loops: Vec<(&'static str, JoinHandle<()>)>,
    jobs: Option<JobsSetup>,
    events: Option<EventHub>,
    runtime: Option<DbRuntime>,
    data_lock: Option<DataLock>,
}

impl Background {
    /// Empty set with the shutdown bound.
    pub fn new(grace: Duration) -> Self {
        let (stop, _) = watch::channel(false);
        Self {
            grace,
            stop,
            loops: Vec::new(),
            jobs: None,
            events: None,
            runtime: None,
            data_lock: None,
        }
    }

    /// The stop signal loops watch: flips to `true` once, at shutdown.
    pub fn stop_signal(&self) -> watch::Receiver<bool> {
        self.stop.subscribe()
    }

    /// Track one loop task under `name`.
    pub fn push(&mut self, name: &'static str, task: JoinHandle<()>) {
        self.loops.push((name, task));
    }

    /// Track several loop tasks.
    pub fn extend(&mut self, tasks: impl IntoIterator<Item = (&'static str, JoinHandle<()>)>) {
        self.loops.extend(tasks);
    }

    /// Cancel this registry's jobs at shutdown.
    #[must_use]
    pub fn with_jobs(mut self, jobs: JobsSetup) -> Self {
        self.jobs = Some(jobs);
        self
    }

    /// End this hub's open event streams the moment shutdown starts, so
    /// connection draining never waits on them.
    #[must_use]
    pub fn with_events(mut self, hub: EventHub) -> Self {
        self.events = Some(hub);
        self
    }

    /// Close this database last at shutdown.
    #[must_use]
    pub fn with_runtime(mut self, runtime: DbRuntime) -> Self {
        self.runtime = Some(runtime);
        self
    }

    /// Release the shared data lock only after the database has closed.
    #[must_use]
    pub fn with_data_lock(mut self, lock: DataLock) -> Self {
        self.data_lock = Some(lock);
        self
    }

    /// Cancel the registry jobs and join the loops, both bounded by the
    /// grace period. A loop still running at the bound is aborted.
    async fn stop_work(&mut self) {
        let grace = self.grace;
        let jobs = self.jobs.clone();
        let cancel_jobs = async move {
            if let Some(jobs) = jobs {
                jobs.cancel_all(grace).await;
            }
        };
        let loops = std::mem::take(&mut self.loops);
        let join_loops =
            futures_util::future::join_all(loops.into_iter().map(|(name, mut task)| async move {
                match tokio::time::timeout(grace, &mut task).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => tracing::warn!(name, %error, "background loop ended early"),
                    Err(_) => {
                        tracing::warn!(
                            name,
                            ?grace,
                            "background loop did not stop in time; aborting it"
                        );
                        task.abort();
                    }
                }
            }));
        tokio::join!(cancel_jobs, join_loops);
    }
}

/// Serve `router` on `listener` until `signal` resolves (or the server
/// fails), then shut down in order. Peers reach handlers as
/// `ConnectInfo<SocketAddr>`.
pub async fn serve(
    listener: TcpListener,
    router: Router,
    mut background: Background,
    signal: impl Future<Output = ()> + Send,
) -> Result<(), ServeError> {
    let mut draining = background.stop_signal();
    let server = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = draining.wait_for(|stopped| *stopped).await;
    });
    let mut server = tokio::spawn(server.into_future());
    let ended_early = tokio::select! {
        () = signal => None,
        outcome = &mut server => Some(outcome),
    };
    // Loops and the HTTP drain hear the signal together. Event streams
    // never end on their own, so they close here, before draining starts.
    background.stop.send_replace(true);
    if let Some(events) = &background.events {
        events.close();
    }
    let grace = background.grace;
    let drain = async {
        if let Some(outcome) = ended_early {
            return outcome;
        }
        match tokio::time::timeout(grace, &mut server).await {
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
    let (served, ()) = tokio::join!(drain, background.stop_work());
    if let Some(runtime) = background.runtime.take() {
        runtime.shutdown().await;
    }
    drop(background.data_lock.take());
    tracing::info!("shutdown complete");
    match served {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(ServeError::Io(error)),
        Err(error) => Err(ServeError::Task(error.to_string())),
    }
}
