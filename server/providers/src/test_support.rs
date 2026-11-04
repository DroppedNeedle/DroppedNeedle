//! What tests need from the providers and production never uses: a
//! scripted wire that answers from a script and records every request,
//! endpoints where nothing listens, unpaced limiters and providers built
//! over a wire of the test's choosing with a cache in a scratch folder.
//!
//! Compiled for this crate's tests and for the `test-support` feature only.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::cache::CacheDb;
use futures_util::future::BoxFuture;

use crate::executor::{Body, Method, Prepared, Url, Wire, WireAnswer, WireError};
use crate::limiter::{LimiterSet, RateLimiter};
use crate::rates::{RATES, Rate, RateRow};
use crate::{ProviderEndpoints, Providers, TransportKind};

/// A scripted wire for tests: answers queue per method and URL (without
/// the query), the last one repeating, and every request is recorded. An
/// unscripted request gets no answer, as if nothing listened.
#[derive(Debug, Default)]
pub struct ScriptedWire {
    script: Mutex<HashMap<(Method, String), VecDeque<Scripted>>>,
    sent: Mutex<Vec<Sent>>,
}

/// One scripted outcome.
#[derive(Debug, Clone)]
pub enum Scripted {
    /// An answer.
    Answer {
        /// The status.
        status: u16,
        /// The headers.
        headers: Vec<(String, String)>,
        /// The body.
        body: Vec<u8>,
    },
    /// No answer, for this reason.
    Transport(TransportKind),
    /// No answer, ever: the deadline decides.
    Hang,
}

/// One request the scripted wire saw.
#[derive(Debug, Clone)]
pub struct Sent {
    /// The method.
    pub method: Method,
    /// The full URL, query included.
    pub url: Url,
    /// The headers.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Body,
}

impl Sent {
    /// One query value.
    #[must_use]
    pub fn query(&self, key: &str) -> Option<String> {
        self.url
            .query_pairs()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.into_owned())
    }
}

impl ScriptedWire {
    /// A wire with nothing scripted.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue `outcome` for `method` requests to `url`.
    pub fn on(&self, method: Method, url: &str, outcome: Scripted) {
        self.script
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .entry((method, url.to_owned()))
            .or_default()
            .push_back(outcome);
    }

    /// Queue an answer for GETs of `url`.
    pub fn answer(
        &self,
        url: &str,
        status: u16,
        headers: &[(&str, &str)],
        body: impl Into<Vec<u8>>,
    ) {
        self.on(
            Method::Get,
            url,
            Scripted::Answer {
                status,
                headers: headers
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                    .collect(),
                body: body.into(),
            },
        );
    }

    /// Every request sent so far, in order.
    #[must_use]
    pub fn sent(&self) -> Vec<Sent> {
        self.sent
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    fn next(&self, method: Method, url: &Url) -> Option<Scripted> {
        let mut bare = url.clone();
        bare.set_query(None);
        let mut script = self
            .script
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let queue = script.get_mut(&(method, bare.to_string()))?;
        if queue.len() > 1 {
            queue.pop_front()
        } else {
            queue.front().cloned()
        }
    }
}

impl Wire for ScriptedWire {
    fn send<'a>(&'a self, prepared: Prepared<'a>) -> BoxFuture<'a, Result<WireAnswer, WireError>> {
        Box::pin(async move {
            let request = prepared.request;
            self.sent
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(Sent {
                    method: request.method,
                    url: prepared.url.clone(),
                    headers: request
                        .headers
                        .iter()
                        .map(|(name, value)| ((*name).to_owned(), value.clone()))
                        .collect(),
                    body: request.body.clone(),
                });
            match self.next(request.method, prepared.url) {
                Some(Scripted::Answer {
                    status,
                    headers,
                    body,
                }) => {
                    if body.len() > prepared.max_body {
                        return Err(WireError::TooLarge);
                    }
                    Ok(WireAnswer {
                        status,
                        headers,
                        body,
                    })
                }
                Some(Scripted::Transport(kind)) => Err(WireError::Transport(kind)),
                Some(Scripted::Hang) => std::future::pending().await,
                None => {
                    tracing::warn!(host = ?prepared.url.host_str(), "unscripted provider request");
                    Err(WireError::Transport(TransportKind::Connect))
                }
            }
        })
    }
}

impl LimiterSet {
    /// One limiter per row, none of them paced. Tests only: production
    /// always paces through [`LimiterSet::new`].
    #[must_use]
    pub fn unpaced() -> Self {
        Self {
            limiters: RATES
                .iter()
                .map(|row| {
                    let unpaced = RateRow {
                        per_second: Rate::Fixed(0.0),
                        ..*row
                    };
                    (row.source, Arc::new(RateLimiter::from_row(&unpaced)))
                })
                .collect(),
        }
    }
}

impl ProviderEndpoints {
    /// Every address on the discard port, where nothing listens, so a
    /// test can never reach a real provider.
    #[must_use]
    pub fn closed() -> Self {
        Self {
            musicbrainz: "http://127.0.0.1:9/musicbrainz/ws/2".to_owned(),
            brainzmash: "http://127.0.0.1:9/brainzmash/ws/2".to_owned(),
            coverart: "http://127.0.0.1:9/coverart".to_owned(),
            listenbrainz: "http://127.0.0.1:9/listenbrainz".to_owned(),
            lastfm: "http://127.0.0.1:9/lastfm/".to_owned(),
            audiodb: "http://127.0.0.1:9/audiodb".to_owned(),
            acoustid: "http://127.0.0.1:9/acoustid".to_owned(),
            discogs: "http://127.0.0.1:9/discogs".to_owned(),
            itunes: "http://127.0.0.1:9/itunes/search".to_owned(),
            deezer: "http://127.0.0.1:9/deezer".to_owned(),
            lrclib: "http://127.0.0.1:9/lrclib".to_owned(),
            wikidata: "http://127.0.0.1:9/wikidata".to_owned(),
            wikipedia: "http://127.0.0.1:9/wikipedia-{lang}".to_owned(),
            commons: "http://127.0.0.1:9/commons".to_owned(),
            ticketmaster: "http://127.0.0.1:9/ticketmaster".to_owned(),
            skiddle: "http://127.0.0.1:9/skiddle".to_owned(),
            geocoding: "http://127.0.0.1:9/geocoding".to_owned(),
            youtube: "http://127.0.0.1:9/youtube".to_owned(),
            github_releases: "http://127.0.0.1:9/releases".to_owned(),
            github_api: "http://127.0.0.1:9/github".to_owned(),
            github_codeload: "http://127.0.0.1:9/codeload".to_owned(),
            spotify_api: "http://127.0.0.1:9/spotify".to_owned(),
            spotify_accounts: "http://127.0.0.1:9/spotify-accounts".to_owned(),
            spotify_cover_cdn: None,
            deezer_images: "http://127.0.0.1:9".to_owned(),
            plex_tv: "http://127.0.0.1:9/plex".to_owned(),
            pwned_passwords: "http://127.0.0.1:9/range".to_owned(),
        }
    }
}

impl Providers {
    /// The providers over `wire`, reading wall time from `clock`, with a
    /// cache in a scratch file. Tests pass a scripted wire or one that
    /// reaches a loopback server.
    #[must_use]
    pub fn over_wire(
        wire: Arc<dyn Wire>,
        clock: Arc<dyn Clock>,
        endpoints: ProviderEndpoints,
    ) -> Self {
        Self::assemble(
            wire,
            clock,
            endpoints,
            LimiterSet::new(),
            CacheDb::scratch(),
        )
    }

    /// The providers over `wire` with every limiter unpaced, for suites
    /// that do not care about pacing: cooldowns and in-flight caps still
    /// hold, rates do not. Every address is on the discard port.
    #[must_use]
    pub fn unpaced(wire: Arc<dyn Wire>) -> Self {
        Self::unpaced_at(wire, ProviderEndpoints::closed())
    }

    /// [`unpaced`](Self::unpaced) with the providers at `endpoints`.
    #[must_use]
    pub fn unpaced_at(wire: Arc<dyn Wire>, endpoints: ProviderEndpoints) -> Self {
        Self::unpaced_on(
            wire,
            Arc::new(droppedneedle_kernel::clock::SystemClock),
            endpoints,
        )
    }

    /// [`unpaced_at`](Self::unpaced_at) reading wall time from `clock`, so
    /// a journey can age the cache.
    #[must_use]
    pub fn unpaced_on(
        wire: Arc<dyn Wire>,
        clock: Arc<dyn Clock>,
        endpoints: ProviderEndpoints,
    ) -> Self {
        Self::assemble(
            wire,
            clock,
            endpoints,
            LimiterSet::unpaced(),
            CacheDb::scratch(),
        )
    }

    /// The providers over a wire nothing answers on, for tests that never
    /// reach a provider through the executor.
    #[must_use]
    pub fn closed() -> Self {
        Self::over_wire(
            Arc::new(ScriptedWire::new()),
            Arc::new(droppedneedle_kernel::clock::SystemClock),
            ProviderEndpoints::closed(),
        )
    }
}
