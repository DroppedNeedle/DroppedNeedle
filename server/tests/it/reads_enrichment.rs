//! Enrichment aggregation over scripted providers: a page still renders
//! when an optional provider is down, a dead MusicBrainz fails identity
//! pages, per-row failures stay on their row, and concerts match cities.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use droppedneedle::reads::enrichment::{
    AggregatingEnrichment, AlbumPageInput, ArtistCore, EnrichmentAggregator, EventCity,
    EventsClient, LastFmAlbumInfo, LastFmArtistInfo, LastFmClient, ListenBrainzClient, LiveEvent,
    LyricsClient, LyricsLookup, LyricsQuery, MatchedConcert, MusicBrainzClient, ProviderError,
    ProviderSource, ReleaseGroupCore, TopRelease, UserConcert,
};
use droppedneedle::reads::search::models::{
    AlbumEnrichmentRequest, ArtistEnrichmentRequest, EnrichmentBatchRequest, EnrichmentSource,
};
use droppedneedle::reads::search::ports::EnrichmentPort;

// ---------------------------------------------------------------------------
// Scripted providers
// ---------------------------------------------------------------------------

/// Scripted MusicBrainz identity.
struct ScriptedMb {
    fail: bool,
}

impl MusicBrainzClient for ScriptedMb {
    fn release_group<'a>(
        &'a self,
        mbid: &'a str,
    ) -> droppedneedle::reads::enrichment::BoxFuture<'a, Result<ReleaseGroupCore, ProviderError>>
    {
        Box::pin(async move {
            if self.fail {
                return Err(ProviderError::new(
                    ProviderSource::MusicBrainz.name(),
                    "connection refused".to_owned(),
                ));
            }
            Ok(ReleaseGroupCore {
                mbid: mbid.to_owned(),
                title: "Dummy".to_owned(),
                artist_name: "Portishead".to_owned(),
                artist_mbid: Some("artist-mbid-1".to_owned()),
                release_date: Some("1994-08-22".to_owned()),
                tags: vec!["trip-hop".to_owned()],
            })
        })
    }

    fn artist_core<'a>(
        &'a self,
        mbid: &'a str,
    ) -> droppedneedle::reads::enrichment::BoxFuture<'a, Result<ArtistCore, ProviderError>> {
        Box::pin(async move {
            if self.fail {
                return Err(ProviderError::new(
                    ProviderSource::MusicBrainz.name(),
                    "connection refused".to_owned(),
                ));
            }
            Ok(ArtistCore {
                mbid: mbid.to_owned(),
                name: "Portishead".to_owned(),
                country: Some("GB".to_owned()),
            })
        })
    }
}

/// Scripted ListenBrainz popularity with a batch call counter.
struct ScriptedLb {
    available: bool,
    fail_batch: bool,
    fail_artists: HashSet<String>,
    batch: HashMap<String, i64>,
    batch_calls: Mutex<usize>,
}

impl ScriptedLb {
    fn healthy() -> Self {
        Self {
            available: true,
            fail_batch: false,
            fail_artists: HashSet::new(),
            batch: HashMap::from([
                ("rg-mbid-1".to_owned(), 90_000),
                ("rg-mbid-2".to_owned(), 12_000),
            ]),
            batch_calls: Mutex::new(0),
        }
    }
}

impl ListenBrainzClient for ScriptedLb {
    fn is_available(&self) -> bool {
        self.available
    }

    fn artist_top_release_groups<'a>(
        &'a self,
        mbid: &'a str,
        count: usize,
    ) -> droppedneedle::reads::enrichment::BoxFuture<'a, Result<Vec<TopRelease>, ProviderError>>
    {
        Box::pin(async move {
            if self.fail_artists.contains(mbid) {
                return Err(ProviderError::new(
                    ProviderSource::ListenBrainz.name(),
                    "timeout".to_owned(),
                ));
            }
            Ok(vec![
                TopRelease {
                    mbid: "rg-a".to_owned(),
                    listen_count: 3_000_000,
                },
                TopRelease {
                    mbid: "rg-b".to_owned(),
                    listen_count: 1_000_000,
                },
            ]
            .into_iter()
            .take(count)
            .collect())
        })
    }

    fn release_group_popularity_batch<'a>(
        &'a self,
        mbids: &'a [String],
    ) -> droppedneedle::reads::enrichment::BoxFuture<'a, Result<HashMap<String, i64>, ProviderError>>
    {
        Box::pin(async move {
            if let Ok(mut calls) = self.batch_calls.lock() {
                *calls += 1;
            }
            if self.fail_batch {
                return Err(ProviderError::new(
                    ProviderSource::ListenBrainz.name(),
                    "500 from upstream".to_owned(),
                ));
            }
            Ok(mbids
                .iter()
                .filter_map(|mbid| self.batch.get(mbid).map(|count| (mbid.clone(), *count)))
                .collect())
        })
    }
}

/// Scripted Last.fm prose and counts.
struct ScriptedLfm {
    available: bool,
    fail: bool,
}

impl LastFmClient for ScriptedLfm {
    fn is_available(&self) -> bool {
        self.available
    }

    fn artist_info<'a>(
        &'a self,
        name: &'a str,
        _mbid: &'a str,
    ) -> droppedneedle::reads::enrichment::BoxFuture<
        'a,
        Result<Option<LastFmArtistInfo>, ProviderError>,
    > {
        Box::pin(async move {
            if self.fail {
                return Err(ProviderError::new(
                    ProviderSource::LastFm.name(),
                    "timeout".to_owned(),
                ));
            }
            Ok(Some(LastFmArtistInfo {
                bio_summary: Some(format!("{name} are a band.")),
                tags: vec!["trip-hop".to_owned()],
                listeners: Some(2_500_000),
                playcount: Some(40_000_000),
                url: Some("https://last.fm/artist".to_owned()),
                mbid: Some("artist-mbid-1".to_owned()),
            }))
        })
    }

    fn album_info<'a>(
        &'a self,
        _artist: &'a str,
        _album: &'a str,
        _mbid: Option<&'a str>,
    ) -> droppedneedle::reads::enrichment::BoxFuture<
        'a,
        Result<Option<LastFmAlbumInfo>, ProviderError>,
    > {
        Box::pin(async move {
            if self.fail {
                return Err(ProviderError::new(
                    ProviderSource::LastFm.name(),
                    "timeout".to_owned(),
                ));
            }
            Ok(Some(LastFmAlbumInfo {
                summary: Some("A landmark record.".to_owned()),
                tags: vec!["trip-hop".to_owned(), "downtempo".to_owned()],
                listeners: Some(800_000),
                playcount: Some(9_000_000),
                url: Some("https://last.fm/album".to_owned()),
            }))
        })
    }
}

/// Scripted LRCLIB lyrics.
struct ScriptedLyrics {
    fail: bool,
    lookup: LyricsLookup,
    seen: Mutex<Vec<String>>,
}

impl LyricsClient for ScriptedLyrics {
    fn exact_lyrics<'a>(
        &'a self,
        query: &'a LyricsQuery,
    ) -> droppedneedle::reads::enrichment::BoxFuture<'a, Result<LyricsLookup, ProviderError>> {
        Box::pin(async move {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push(format!("{} - {}", query.artist, query.title));
            }
            if self.fail {
                return Err(ProviderError::new(
                    ProviderSource::Lrclib.name(),
                    "connection reset".to_owned(),
                ));
            }
            Ok(LyricsLookup {
                found: self.lookup.found,
                plain: self.lookup.plain.clone(),
                synced: self.lookup.synced.clone(),
            })
        })
    }
}

/// Scripted concerts feed.
struct ScriptedEvents {
    fail: bool,
    concerts: Vec<UserConcert>,
    cities: Vec<EventCity>,
}

impl EventsClient for ScriptedEvents {
    fn concerts_for_user<'a>(
        &'a self,
        _user_id: &'a str,
    ) -> droppedneedle::reads::enrichment::BoxFuture<'a, Result<Vec<UserConcert>, ProviderError>>
    {
        Box::pin(async move {
            if self.fail {
                return Err(ProviderError::new(
                    ProviderSource::EventsFeed.name(),
                    "feed unavailable".to_owned(),
                ));
            }
            Ok(self.concerts.clone())
        })
    }

    fn cities<'a>(
        &'a self,
        _user_id: &'a str,
    ) -> droppedneedle::reads::enrichment::BoxFuture<'a, Result<Vec<EventCity>, ProviderError>>
    {
        Box::pin(async move {
            if self.fail {
                return Err(ProviderError::new(
                    ProviderSource::EventsFeed.name(),
                    "feed unavailable".to_owned(),
                ));
            }
            Ok(self.cities.clone())
        })
    }
}

fn live_event(city: Option<&str>, lat: Option<f64>, lon: Option<f64>, id: &str) -> LiveEvent {
    LiveEvent {
        source: "ticketmaster".to_owned(),
        source_event_id: id.to_owned(),
        artist_name: "Portishead".to_owned(),
        event_name: "Portishead live".to_owned(),
        local_date: "2026-10-10".to_owned(),
        venue_name: Some("Arena".to_owned()),
        city: city.map(str::to_owned),
        latitude: lat,
        longitude: lon,
        ticket_url: None,
    }
}

fn bristol() -> EventCity {
    EventCity {
        city_name: "Bristol".to_owned(),
        latitude: 51.4545,
        longitude: -2.5879,
        radius_km: 50.0,
    }
}

// ---------------------------------------------------------------------------
// Album-page enrichment
// ---------------------------------------------------------------------------

fn album_aggregator(mb_fail: bool, lb: ScriptedLb, lfm_fail: bool) -> EnrichmentAggregator {
    EnrichmentAggregator::new(
        std::sync::Arc::new(ScriptedMb { fail: mb_fail }),
        std::sync::Arc::new(lb),
        std::sync::Arc::new(ScriptedLfm {
            available: true,
            fail: lfm_fail,
        }),
        std::sync::Arc::new(ScriptedLyrics {
            fail: false,
            lookup: LyricsLookup {
                found: false,
                plain: None,
                synced: None,
            },
            seen: Mutex::new(Vec::new()),
        }),
        std::sync::Arc::new(ScriptedEvents {
            fail: false,
            concerts: Vec::new(),
            cities: Vec::new(),
        }),
    )
}

fn album_input() -> AlbumPageInput {
    AlbumPageInput {
        rg_mbid: "rg-mbid-1".to_owned(),
        artist_name: "Portishead".to_owned(),
        album_title: "Dummy".to_owned(),
    }
}

#[tokio::test]
async fn album_page_completes_with_one_provider_down() {
    let aggregator = album_aggregator(false, ScriptedLb::healthy(), true);
    let page = aggregator
        .enrich_album_page(album_input())
        .await
        .expect("one dead leg degrades, never fails");
    assert_eq!(page.identity.title, "Dummy");
    assert_eq!(page.listen_count, Some(90_000));
    assert_eq!(page.bio, None);
    assert!(page.tags.is_empty());
    assert_eq!(page.degradations.len(), 1);
    assert_eq!(page.degradations[0].source, "lastfm");
    assert_eq!(page.degradations[0].code, "ENRICHMENT_UNAVAILABLE");
}

#[tokio::test]
async fn album_page_fails_when_musicbrainz_dead() {
    let aggregator = album_aggregator(true, ScriptedLb::healthy(), false);
    let error = aggregator
        .enrich_album_page(album_input())
        .await
        .expect_err("identity-critical failure must fail the page");
    assert_eq!(error.to_string(), "musicbrainz identity unavailable");
}

// ---------------------------------------------------------------------------
// Search batch through the enrichment port
// ---------------------------------------------------------------------------

fn batch_request() -> EnrichmentBatchRequest {
    EnrichmentBatchRequest {
        artists: vec![
            ArtistEnrichmentRequest {
                musicbrainz_id: "artist-mbid-1".to_owned(),
                name: "Portishead".to_owned(),
            },
            ArtistEnrichmentRequest {
                musicbrainz_id: "artist-mbid-2".to_owned(),
                name: "Massive Attack".to_owned(),
            },
        ],
        albums: vec![AlbumEnrichmentRequest {
            musicbrainz_id: "rg-mbid-1".to_owned(),
            artist_name: "Portishead".to_owned(),
            album_name: "Dummy".to_owned(),
        }],
    }
}

#[tokio::test]
async fn per_item_failure_degrades_only_its_row() {
    let lb = ScriptedLb {
        fail_artists: HashSet::from(["artist-mbid-2".to_owned()]),
        ..ScriptedLb::healthy()
    };
    let port = AggregatingEnrichment::new(std::sync::Arc::new(album_aggregator(false, lb, false)));
    let response = port
        .enrich_batch(batch_request())
        .await
        .expect("batch holds");
    assert_eq!(response.artists[0].listen_count, Some(4_000_000));
    assert_eq!(response.artists[1].listen_count, None);
    assert_eq!(response.albums[0].listen_count, Some(90_000));
    assert_eq!(response.degradations.len(), 1);
}

#[tokio::test]
async fn search_batch_falls_back_to_lastfm() {
    let unavailable = ScriptedLb {
        available: false,
        ..ScriptedLb::healthy()
    };
    let port = AggregatingEnrichment::new(std::sync::Arc::new(album_aggregator(
        false,
        unavailable,
        false,
    )));
    let response = port
        .enrich_batch(batch_request())
        .await
        .expect("batch holds");
    assert_eq!(response.source, EnrichmentSource::Lastfm);
    assert_eq!(response.artists[0].listen_count, Some(2_500_000));
    assert!(response.degradations.is_empty());
}

// ---------------------------------------------------------------------------
// Events lookup
// ---------------------------------------------------------------------------

fn events_aggregator(
    concerts: Vec<UserConcert>,
    cities: Vec<EventCity>,
    fail: bool,
) -> EnrichmentAggregator {
    EnrichmentAggregator::new(
        std::sync::Arc::new(ScriptedMb { fail: false }),
        std::sync::Arc::new(ScriptedLb::healthy()),
        std::sync::Arc::new(ScriptedLfm {
            available: true,
            fail: false,
        }),
        std::sync::Arc::new(ScriptedLyrics {
            fail: false,
            lookup: LyricsLookup {
                found: false,
                plain: None,
                synced: None,
            },
            seen: Mutex::new(Vec::new()),
        }),
        std::sync::Arc::new(ScriptedEvents {
            fail,
            concerts,
            cities,
        }),
    )
}

#[tokio::test]
async fn events_lookup_matches_cities() {
    let concerts = vec![
        UserConcert {
            event: live_event(Some("Bristol"), Some(51.4545), Some(-2.5879), "ev-near"),
            artist_mbid: "artist-mbid-1".to_owned(),
        },
        UserConcert {
            event: live_event(Some("Bristol"), None, None, "ev-name"),
            artist_mbid: "artist-mbid-1".to_owned(),
        },
        UserConcert {
            event: live_event(Some("Tokyo"), Some(35.6762), Some(139.6503), "ev-far"),
            artist_mbid: "artist-mbid-1".to_owned(),
        },
    ];
    let outcome = events_aggregator(concerts, vec![bristol()], false)
        .lookup_events("user-1")
        .await;
    assert!(outcome.degradation.is_none());
    let ids: Vec<&str> = outcome
        .concerts
        .iter()
        .map(|row| row.event.source_event_id.as_str())
        .collect();
    assert_eq!(ids, vec!["ev-near", "ev-name"]);
    assert_eq!(outcome.concerts[0].distance_km, Some(0.0));
    assert_eq!(outcome.concerts[0].matched_city, "Bristol");
    let name_match: &MatchedConcert = &outcome.concerts[1];
    assert_eq!(name_match.distance_km, None);

    let empty = events_aggregator(Vec::new(), Vec::new(), false)
        .lookup_events("user-nocities")
        .await;
    assert!(empty.concerts.is_empty());
    assert!(empty.degradation.is_none());
}
