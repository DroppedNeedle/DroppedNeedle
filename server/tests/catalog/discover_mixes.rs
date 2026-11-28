//! Daily Mixes over a real database and Discover as the production builder
//! wires it: each person's mixes are built by their job from their own
//! taste groups and library tracks, kept whole, read back by the page with
//! no provider read, and forgotten with their history. Track sounds are
//! written straight into the analysis table, each vector along an angle in
//! one plane, so which tracks sound alike can be worked out by hand.

use std::sync::Arc;

use droppedneedle_catalog::discover::mixes::store::kept;
use droppedneedle_catalog::discover::models::{DailyMix, MixTrackKind};
use droppedneedle_catalog::discover::services::DiscoverDeps;
use droppedneedle_kernel::db::{DbError, Lane};
use droppedneedle_kernel::runtime_config::sections::SoundAnalysisSettings;
use droppedneedle_providers::Providers;
use serde_json::Value;

use crate::common::auth::AuthRig;
use crate::discover_page::settled_page;
use crate::discover_queue::{Script, discover_deps, discover_routes, insert};

fn mbid(n: usize) -> String {
    format!("{n:08x}-0000-4000-8000-000000000000")
}

/// One planted library track.
struct Planted {
    id: String,
    /// Its artist, by number: each has a library row and a MusicBrainz id.
    artist: usize,
    title: String,
    /// Its ReplayGain track gain: the higher, the quieter.
    gain: f64,
    /// The angle its sound points along, when it is analysed.
    angle: Option<f64>,
}

fn planted(id: &str, artist: usize, gain: f64, angle: Option<f64>) -> Planted {
    Planted {
        id: id.to_owned(),
        artist,
        title: id.to_owned(),
        gain,
        angle,
    }
}

/// `count` tracks by `artist`, each on the artist's album.
fn by(artist: usize, count: usize, angle: Option<f64>) -> Vec<Planted> {
    (0..count)
        .map(|n| {
            planted(
                &format!("a{artist}-t{n}"),
                artist,
                -(n as f64) - artist as f64 / 10.0,
                angle.map(|angle| angle + n as f64),
            )
        })
        .collect()
}

fn at_angle(degrees: f64) -> Vec<u8> {
    let radians = degrees.to_radians();
    let mut vector = vec![0.0f32; 200];
    vector[0] = radians.cos() as f32;
    vector[1] = radians.sin() as f32;
    vector
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

/// Two people, and `tracks` in the library, each on an album of its own
/// artist, analysed where it has an angle.
async fn plant(rig: &AuthRig, tracks: Vec<Planted>) {
    rig.platform
        .db
        .write(Lane::Foreground, "test.plant", move |tx| {
            tx.execute_batch(
                "INSERT OR IGNORE INTO auth_users (id, display_name, role, created_at) \
                 VALUES ('user-1', 'Ada', 'user', 0), ('user-2', 'Bo', 'user', 0);",
            )?;
            for track in &tracks {
                let artist = format!("artist-{}", track.artist);
                tx.execute(
                    "INSERT OR IGNORE INTO local_artists (id, display_name, folded_name, kind, \
                     created_at, updated_at) VALUES (?1, ?1, ?1, 'group', 1, 1)",
                    [&artist],
                )?;
                tx.execute(
                    "INSERT OR IGNORE INTO local_artist_external_identities (local_artist_id, \
                     provider, provider_artist_id, decision_source, selected_at) \
                     VALUES (?1, 'musicbrainz', ?2, 'manual', 1)",
                    [&artist, &mbid(track.artist)],
                )?;
                let album = format!("album-{}", track.artist);
                tx.execute(
                    "INSERT OR IGNORE INTO local_albums (id, root_id, grouping_key, title, \
                     title_folded, album_artist_id, grouping_source, created_at, updated_at) \
                     VALUES (?1, 'r1', ?1, ?1, ?1, ?2, 'automatic', 1, 1)",
                    [&album, &artist],
                )?;
                tx.execute(
                    "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, \
                     relative_path, path_hash, file_size_bytes, file_mtime_ns, stat_revision, \
                     title, title_folded, artist_name, album_title, album_title_folded, \
                     file_format, availability, ingest_source, imported_at, membership_source, \
                     replaygain_track_gain) \
                     VALUES (?1, ?2, 'r1', '/music/' || ?1, ?1, ?1, 1, 1, 'stat-' || ?1, ?3, ?3, \
                     ?4, ?2, ?2, 'flac', 'indexed', 'scan', 1, 'automatic', ?5)",
                    rusqlite::params![track.id, album, track.title, artist, track.gain],
                )?;
                tx.execute(
                    "INSERT INTO local_track_artists (local_track_id, position, local_artist_id, \
                     role) VALUES (?1, 0, ?2, 'main')",
                    [&track.id, &artist],
                )?;
                if let Some(angle) = track.angle {
                    let tags: Vec<u8> = (0..50).map(|tag| if tag < 5 { 200 } else { 10 }).collect();
                    tx.execute(
                        "INSERT INTO library_sound_analyses (local_track_id, analysis_version, \
                         stat_revision, model, outcome, vector, tags, analysed_seconds, \
                         analysed_at) VALUES (?1, 1, 'stat-' || ?1, 'MSD_musicnn', 'analysed', \
                         ?2, ?3, 1.0, 1)",
                        rusqlite::params![track.id, at_angle(angle), tags],
                    )?;
                }
            }
            Ok::<_, DbError>(())
        })
        .await
        .expect("library planted");
}

/// `user`'s taste build with one group a list of `(artist, plays)`, the
/// group's share and its anchor the first artist.
fn taste(user: &str, groups: &[(f64, &[(usize, i64)])]) -> String {
    let mut sql = format!(
        "INSERT INTO catalog_taste_builds (user_id, built_at, plays_counted, \
         imported_listens_counted, artists_played, seeds, incomplete) \
         VALUES ('{user}', 1, 1, 0, 1, 1, 0);"
    );
    for (group_no, (share, artists)) in groups.iter().enumerate() {
        sql.push_str(&format!(
            "INSERT INTO catalog_taste_groups (user_id, group_no, anchor_artist_mbid, \
             anchor_name, share, recent_share, trend) VALUES ('{user}', {group_no}, '{}', \
             'Artist {}', {share}, {share}, 'steady');",
            mbid(artists[0].0),
            artists[0].0
        ));
        for (artist, plays) in artists.iter() {
            sql.push_str(&format!(
                "INSERT INTO catalog_taste_artists (user_id, artist_mbid_lower, artist_mbid, \
                 name, plays, imported_listens, months_active, last_played_at, weight, \
                 is_seed, group_no) VALUES ('{user}', '{m}', '{m}', 'Artist {artist}', \
                 {plays}, 0, 1, NULL, 1.0, 1, {group_no});",
                m = mbid(*artist)
            ));
        }
    }
    sql
}

fn sound_on(rig: &AuthRig, on: bool) {
    rig.config
        .save_blocking(SoundAnalysisSettings {
            sound_analysis_enabled: on,
            sound_analysis_paused: false,
        })
        .expect("sound analysis saved");
}

fn deps(rig: &AuthRig, dir: &crate::common::ScratchDir) -> DiscoverDeps {
    discover_deps(rig, dir, Providers::closed(), |_| {})
}

async fn build(deps: &DiscoverDeps, user: &str) {
    deps.mixes
        .run_once(user, |_| Ok(()))
        .await
        .expect("mixes built");
}

async fn mixes(rig: &AuthRig, user: &str) -> Vec<DailyMix> {
    kept(&rig.platform.db, user).await.expect("mixes read")
}

/// The MusicBrainz ids of a mix's tracks' artists.
fn artists(mix: &DailyMix) -> Vec<String> {
    mix.tracks
        .iter()
        .filter_map(|track| track.artist_mbid.clone())
        .collect()
}

/// A mix's stated counts are its rows, no artist has more than three
/// tracks, and no three in a row are by one artist.
fn well_formed(mix: &DailyMix) {
    let new = mix
        .tracks
        .iter()
        .filter(|track| track.kind == MixTrackKind::New)
        .count();
    assert_eq!(mix.new as usize, new, "{}", mix.name);
    assert_eq!(mix.yours as usize, mix.tracks.len() - new, "{}", mix.name);
    let artists = artists(mix);
    for artist in &artists {
        assert!(
            artists.iter().filter(|other| *other == artist).count() <= 3,
            "{artist} has more than three tracks on {}",
            mix.name
        );
    }
    assert!(
        artists
            .windows(3)
            .all(|run| !(run[0] == run[1] && run[1] == run[2])),
        "three in a row on {}: {artists:?}",
        mix.name
    );
}

/// Two people's mixes come from their own groups: each holds only their
/// group's artists, and is named by the two of them they play most. One
/// person's plays change nothing in the other's mix, and building again on
/// the same day makes the same mix. With sound analysis off, a mix is
/// still built, and no track on it is marked as found by sound.
#[tokio::test]
async fn each_person_gets_mixes_from_their_own_tastes() {
    let rig = AuthRig::new("mixes-own").await;
    let mut tracks = Vec::new();
    for artist in 1..=8 {
        tracks.extend(by(artist, 3, None));
    }
    plant(&rig, tracks).await;
    insert(
        &rig.platform.db,
        taste("user-1", &[(1.0, &[(1, 50), (2, 30), (3, 5), (4, 1)])])
            + &taste("user-2", &[(1.0, &[(5, 9), (6, 40), (7, 2), (8, 1)])]),
    )
    .await;
    sound_on(&rig, false);
    let dir = crate::common::ScratchDir::new("mixes-own");
    let deps = deps(&rig, &dir);
    build(&deps, "user-1").await;
    build(&deps, "user-2").await;

    let ada = mixes(&rig, "user-1").await;
    let bo = mixes(&rig, "user-2").await;
    assert_eq!((ada.len(), bo.len()), (1, 1));
    assert_eq!(ada[0].name, "Artist 1, Artist 2 and more");
    assert_eq!(bo[0].name, "Artist 6, Artist 5 and more");
    let own = |range: std::ops::RangeInclusive<usize>| -> Vec<String> { range.map(mbid).collect() };
    assert!(
        artists(&ada[0])
            .iter()
            .all(|artist| own(1..=4).contains(artist))
    );
    assert!(
        artists(&bo[0])
            .iter()
            .all(|artist| own(5..=8).contains(artist))
    );
    for mix in ada.iter().chain(&bo) {
        well_formed(mix);
        assert_eq!(
            mix.tracks.len(),
            12,
            "every track of the group's four artists"
        );
        assert!(
            mix.tracks
                .iter()
                .all(|track| track.kind == MixTrackKind::Artist && track.local_track_id.is_some()),
            "with analysis off nothing is found by sound"
        );
    }

    // Bo plays Ada's artists' tracks; Ada's mix stays as it was.
    let plays: String = (0..3)
        .map(|n| {
            format!(
                "INSERT INTO library_play_history (id, user_id, local_track_id, track_name, \
                 artist_name, played_at) VALUES ('p{n}', 'user-2', 'a4-t{n}', 'Song', \
                 'Someone', 1000);"
            )
        })
        .collect();
    insert(&rig.platform.db, plays).await;
    build(&deps, "user-1").await;
    assert_eq!(
        mixes(&rig, "user-1").await,
        ada,
        "the same day, the same mix"
    );
}

/// With sound analysis on, a mix takes library tracks by other artists
/// that sound like its group's, never one past the cut-off, and keeps one
/// version of each song and three tracks an artist. Turned off, the same
/// mix holds only the group's own artists.
#[tokio::test]
async fn sound_finds_are_library_tracks_near_the_tastes_sound() {
    let rig = AuthRig::new("mixes-sound").await;
    let mut tracks = vec![
        planted("song", 1, -1.0, Some(0.0)),
        Planted {
            title: "Song (Remastered)".to_owned(),
            ..planted("song-again", 1, -2.0, Some(0.5))
        },
    ];
    // Artist 1 has only the two versions, so only the versions rule keeps
    // one of them out.
    tracks.extend(by(2, 3, Some(2.0)));
    tracks.extend(by(10, 3, Some(-2.0)));
    tracks.extend(by(11, 3, Some(1.0)));
    // Other artists close to the group's sound, and one far from it.
    for artist in 3..=6 {
        tracks.extend(by(artist, 2, Some(artist as f64 * 2.0)));
    }
    tracks.push(planted("far", 9, -3.0, Some(70.0)));
    plant(&rig, tracks).await;
    insert(
        &rig.platform.db,
        taste("user-1", &[(1.0, &[(1, 50), (2, 30), (10, 5), (11, 4)])]),
    )
    .await;
    sound_on(&rig, true);
    let dir = crate::common::ScratchDir::new("mixes-sound");
    let deps = deps(&rig, &dir);
    build(&deps, "user-1").await;

    let mix = mixes(&rig, "user-1").await.remove(0);
    well_formed(&mix);
    let group = [mbid(1), mbid(2), mbid(10), mbid(11)];
    let found: Vec<_> = mix
        .tracks
        .iter()
        .filter(|track| track.kind == MixTrackKind::Sound)
        .collect();
    assert!(!found.is_empty(), "{mix:?}");
    for track in &found {
        assert!(
            track.local_track_id.is_some(),
            "a sound find is a library track"
        );
        assert!(track.album.in_library);
        let artist = track.artist_mbid.clone().unwrap_or_default();
        assert!(
            !group.contains(&artist),
            "{artist} is one of the group's own"
        );
    }
    let titles: Vec<&str> = mix
        .tracks
        .iter()
        .map(|track| track.title.as_str())
        .collect();
    assert!(
        !titles.contains(&"far"),
        "nothing past the cut-off: {titles:?}"
    );
    assert!(
        !(titles.contains(&"song") && titles.contains(&"Song (Remastered)")),
        "one version of a song: {titles:?}"
    );

    sound_on(&rig, false);
    build(&deps, "user-1").await;
    let mix = mixes(&rig, "user-1").await.remove(0);
    well_formed(&mix);
    assert!(
        mix.tracks
            .iter()
            .all(|track| track.kind == MixTrackKind::Artist),
        "with analysis off the mix holds the group's own artists"
    );
}

/// A person with no taste yet has no mixes, and the page says why. A group
/// they said was a phase gets none; one they asked for less from comes
/// after the rest.
#[tokio::test]
async fn mixes_follow_what_the_person_said_about_their_tastes() {
    let rig = AuthRig::new("mixes-said").await;
    let mut tracks = Vec::new();
    for artist in 1..=8 {
        tracks.extend(by(artist, 3, None));
    }
    plant(&rig, tracks).await;
    sound_on(&rig, false);
    let dir = crate::common::ScratchDir::new("mixes-said");
    let script = Arc::new(Script::default());
    let deps = discover_deps(&rig, &dir, Providers::closed(), {
        let script = Arc::clone(&script);
        move |network| *network = script
    });
    build(&deps, "user-1").await;
    assert!(mixes(&rig, "user-1").await.is_empty());
    let page = settled_page(&rig, &discover_routes(deps.clone()), "the page").await;
    let state = page["section_states"]
        .as_array()
        .expect("states")
        .iter()
        .find(|state| state["key"] == "daily_mixes")
        .cloned();
    assert_eq!(
        state.as_ref().map(|state| state["state"].clone()),
        Some(Value::from("needs_taste")),
        "{page}"
    );

    insert(
        &rig.platform.db,
        taste(
            "user-1",
            &[
                (0.6, &[(1, 9), (2, 8), (3, 7), (4, 6)]),
                (0.4, &[(5, 9), (6, 8), (7, 7), (8, 6)]),
            ],
        ),
    )
    .await;
    let say = |kind: &str, anchor: usize| {
        format!(
            "INSERT INTO catalog_discover_tunings (user_id, kind, target, target_name, \
             group_anchor_mbid, group_label, created_at) VALUES ('user-1', '{kind}', '{m}', \
             'Artist {anchor}', '{m}', 'Artist {anchor}', 1);",
            m = mbid(anchor)
        )
    };
    build(&deps, "user-1").await;
    let names =
        |mixes: &[DailyMix]| -> Vec<String> { mixes.iter().map(|mix| mix.name.clone()).collect() };
    assert_eq!(
        names(&mixes(&rig, "user-1").await),
        ["Artist 1, Artist 2 and more", "Artist 5, Artist 6 and more"]
    );
    insert(&rig.platform.db, say("less_from_seed", 1)).await;
    build(&deps, "user-1").await;
    assert_eq!(
        names(&mixes(&rig, "user-1").await),
        ["Artist 5, Artist 6 and more", "Artist 1, Artist 2 and more"],
        "less from a group puts it last, never removes it"
    );
    insert(&rig.platform.db, say("phase", 5)).await;
    build(&deps, "user-1").await;
    assert_eq!(
        names(&mixes(&rig, "user-1").await),
        ["Artist 1, Artist 2 and more"],
        "a phase gets no mix"
    );
}

/// The page shows the mixes the job kept and asks no provider for them:
/// with every other section switched off, the build reads nothing from the
/// listening services.
#[tokio::test]
async fn the_page_reads_the_kept_mixes_and_no_provider() {
    let rig = AuthRig::new("mixes-page").await;
    let mut tracks = Vec::new();
    for artist in 1..=4 {
        tracks.extend(by(artist, 3, None));
    }
    plant(&rig, tracks).await;
    insert(
        &rig.platform.db,
        taste("user-1", &[(1.0, &[(1, 9), (2, 8), (3, 7), (4, 6)])]),
    )
    .await;
    let off: String = [
        "weekly_set",
        "weekly_exploration",
        "playlist_discovery",
        "radio_sections",
        "new_from_your_artists",
        "gaps_in_your_artists",
        "from_your_shelves",
        "genre_list",
    ]
    .iter()
    .map(|key| {
        format!(
            "INSERT INTO app_section_prefs (user_id, page, section_key, enabled, updated_at) \
             VALUES ('user-1', 'discover', '{key}', 0, 1);"
        )
    })
    .collect();
    insert(&rig.platform.db, off).await;
    sound_on(&rig, false);
    let dir = crate::common::ScratchDir::new("mixes-page");
    let script = Arc::new(Script::default());
    let deps = discover_deps(&rig, &dir, Providers::closed(), {
        let script = Arc::clone(&script);
        move |network| *network = script
    });
    build(&deps, "user-1").await;
    // The page serves the mix as kept, even one the job would not make.
    insert(
        &rig.platform.db,
        "UPDATE catalog_mixes SET name = 'Kept as stored' WHERE user_id = 'user-1'; \
         DELETE FROM catalog_mix_tracks WHERE position > 0;"
            .to_owned(),
    )
    .await;
    let stored = mixes(&rig, "user-1").await;
    assert_eq!(stored.len(), 1);
    assert_eq!(
        (stored[0].name.as_str(), stored[0].tracks.len()),
        ("Kept as stored", 1)
    );

    let page = settled_page(&rig, &discover_routes(deps), "the page").await;
    let section = page["sections"]
        .as_array()
        .expect("sections")
        .iter()
        .find(|section| section["key"] == "daily_mixes")
        .cloned()
        .expect("the mixes are on the page");
    assert_eq!(section["content"]["shape"], "mixes");
    let shown: Vec<DailyMix> =
        serde_json::from_value(section["content"]["mixes"].clone()).expect("mixes read");
    assert_eq!(shown, stored);
    use std::sync::atomic::Ordering::SeqCst;
    let reads = [
        script.all_time_reads.load(SeqCst),
        script.user_artist_reads.load(SeqCst),
        script.similar_reads.load(SeqCst),
        script.user_album_reads.load(SeqCst),
        script.listener_reads.load(SeqCst),
        script.fresh_reads.load(SeqCst),
        script.discography_reads.lock().expect("reads").len(),
    ];
    assert_eq!(reads, [0; 7], "the page build read nothing for the mixes");
}

/// Forget my history takes the person's mixes, and a build of theirs
/// claimed before the forget writes nothing after it.
#[tokio::test]
async fn forget_takes_the_mixes_and_stops_a_running_build() {
    let rig = AuthRig::new("mixes-forget").await;
    let mut tracks = Vec::new();
    for artist in 1..=4 {
        tracks.extend(by(artist, 3, None));
    }
    plant(&rig, tracks).await;
    insert(
        &rig.platform.db,
        taste("user-1", &[(1.0, &[(1, 9), (2, 8), (3, 7), (4, 6)])]),
    )
    .await;
    sound_on(&rig, false);
    let dir = crate::common::ScratchDir::new("mixes-forget");
    let deps = deps(&rig, &dir);
    build(&deps, "user-1").await;
    assert_eq!(mixes(&rig, "user-1").await.len(), 1);

    // The forget also takes the taste the mixes are built from: it is put
    // back before the run writes, so only the forget keeps it from writing.
    let again = insert(
        &rig.platform.db,
        taste("user-1", &[(1.0, &[(1, 9), (2, 8), (3, 7), (4, 6)])]),
    );
    droppedneedle_catalog::test_support::mixes_across_a_forget(&deps, "user-1", again)
        .await
        .expect("the run ends");
    assert!(
        mixes(&rig, "user-1").await.is_empty(),
        "the mixes went, and a build claimed before the forget kept nothing"
    );
    build(&deps, "user-1").await;
    assert_eq!(mixes(&rig, "user-1").await.len(), 1, "a later build keeps");
}

fn group_of(n: usize) -> String {
    format!("{n:08x}-1111-4111-8111-111111111111")
}

/// Album `n` by the new artist in `user`'s pool, proposed for their first
/// taste group through Artist 1.
fn candidate(user: &str, rank: usize, n: usize) -> String {
    format!(
        "INSERT INTO catalog_discover_candidates (user_id, release_group_mbid_lower, rank, \
         kind, title, artist_mbid, artist_name, credited_artist_mbids, primary_type, \
         first_release_date, album_listeners, artist_listeners, support, group_no, \
         group_support) VALUES ('{user}', '{g}', {rank}, 'new_artist', 'Album {n}', \
         '{artist}', 'Newcomer', '[]', 'Album', '2010-05-01', 5000, 50000, 10.0, 0, 10.0); \
         INSERT INTO catalog_discover_candidate_paths (user_id, release_group_mbid_lower, \
         path_no, kind, source, fetched_at, artist_mbid, artist_name, artist_plays, \
         artist_weight, artist_albums_owned, link_strength, link_rank, link_of, \
         named_by_labs, named_by_lastfm) VALUES ('{user}', '{g}', 0, 'seed', \
         'listenbrainz_labs', 1, '{seed}', 'Artist 1', 5, 1.0, 0, 0.8, 2, 100, 1, 0);",
        g = group_of(n),
        artist = mbid(20),
        seed = mbid(1),
    )
}

/// With a source set up for what the library lacks, a mix takes songs
/// from the albums the person's pool proposed for the taste, each with its
/// reason, after a track by the artist it was found through. Only an album
/// the library does not hold, nobody has asked for and the person did not
/// hide gives songs, and the read of the artist's songs is counted
/// against the day's background reads.
#[tokio::test]
async fn new_tracks_are_unowned_unrequested_and_not_hidden() {
    use droppedneedle_kernel::runtime_config::Secret;
    use droppedneedle_kernel::runtime_config::secret_sections::YouTubeConnection;
    use droppedneedle_providers::executor::Method;
    use droppedneedle_providers::test_support::{Scripted, ScriptedWire};

    let rig = AuthRig::new("mixes-new").await;
    let mut tracks = Vec::new();
    for artist in 1..=4 {
        tracks.extend(by(artist, 3, None));
    }
    tracks.extend(by(30, 1, None));
    plant(&rig, tracks).await;
    let mut rows = taste("user-1", &[(1.0, &[(1, 9), (2, 8), (3, 7), (4, 6)])]);
    for (rank, n) in (1..=4).enumerate() {
        rows += &candidate("user-1", rank, n);
    }
    // The pool was built after the taste build it stands on.
    rows += "INSERT INTO catalog_discover_pool_builds (user_id, built_at, incomplete, \
         waiting_for, seeds_used, neighbours_found, known_left_out, hubs_left_out, \
         artists_ranked, artists_without_album, new_artist_candidates, source_pick_candidates, \
         gap_candidates, shelf_candidates, fresh_candidates, thread_candidates) VALUES \
         ('user-1', 2, 0, '[]', 1, 1, 0, NULL, 1, 0, 4, 0, 0, 0, 0, 0);";
    rows += &format!(
        "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
         album_title, requested_at, status, user_id) VALUES ('{two}', '{two}', 'Newcomer', \
         'Album 2', 1, 'pending', 'user-2'); \
         INSERT INTO catalog_discover_feedback (user_id, target, target_mbid_lower, kind, \
         created_at) VALUES ('user-1', 'album', '{three}', 'no', 1); \
         INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, decision_source, selected_at) VALUES ('album-30', \
         'musicbrainz', '{four}', 'manual', 1);",
        two = group_of(2),
        three = group_of(3),
        four = group_of(4),
    );
    insert(&rig.platform.db, rows).await;
    crate::common::listening_store(&rig.platform)
        .save_listenbrainz("user-1", "ada-lb", "ada-token")
        .await
        .expect("ListenBrainz link");
    crate::discover_queue::seen_now(&rig, "user-1").await;
    rig.config
        .save_secret_blocking(YouTubeConnection {
            api_key: Secret::new("youtube-key"),
            enabled: true,
            api_enabled: true,
            daily_quota_limit: 10_000,
        })
        .expect("youtube settings save");
    sound_on(&rig, false);

    let wire = Arc::new(ScriptedWire::new());
    let recordings: Vec<Value> = (1..=4)
        .map(|n| {
            serde_json::json!({
                "artist_mbids": [mbid(20)],
                "artist_name": "Newcomer",
                "recording_mbid": format!("{n:08x}-2222-4222-8222-222222222222"),
                "recording_name": format!("Song {n}"),
                "release_mbid": format!("{n:08x}-3333-4333-8333-333333333333"),
                "release_name": format!("Album {n}"),
                "total_listen_count": 100 - n,
                "total_user_count": 10
            })
        })
        .collect();
    wire.answer(
        &format!(
            "http://127.0.0.1:9/listenbrainz/1/popularity/top-recordings-for-artist/{}",
            mbid(20)
        ),
        200,
        &[("content-type", "application/json")],
        Value::Array(recordings).to_string(),
    );
    let groups: serde_json::Map<String, Value> = (1..=4)
        .map(|n| {
            (
                format!("{n:08x}-2222-4222-8222-222222222222"),
                serde_json::json!({"release": {"release_group_mbid": group_of(n)}}),
            )
        })
        .collect();
    wire.on(
        Method::Post,
        "http://127.0.0.1:9/listenbrainz/1/metadata/recording/",
        Scripted::Answer {
            status: 200,
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: Value::Object(groups).to_string().into_bytes(),
        },
    );
    let dir = crate::common::ScratchDir::new("mixes-new");
    let deps = discover_deps(&rig, &dir, Providers::unpaced(wire.clone()), |_| {});
    build(&deps, "user-1").await;

    let mix = mixes(&rig, "user-1").await.remove(0);
    well_formed(&mix);
    let new: Vec<_> = mix
        .tracks
        .iter()
        .filter(|track| track.kind == MixTrackKind::New)
        .collect();
    assert_eq!(
        new.iter()
            .map(|track| track.title.as_str())
            .collect::<Vec<_>>(),
        ["Song 1"],
        "{mix:?}"
    );
    let song = new[0];
    assert_eq!(song.local_track_id, None);
    assert_eq!(
        song.album.musicbrainz_id.as_deref(),
        Some(group_of(1).as_str())
    );
    assert!(
        song.reason
            .as_ref()
            .is_some_and(|reason| reason.line.contains("Artist 1"))
    );
    let at = mix
        .tracks
        .iter()
        .position(|track| track.kind == MixTrackKind::New)
        .expect("placed");
    assert_eq!(
        mix.tracks[at - 1].artist_mbid.as_deref(),
        Some(mbid(1).as_str()),
        "the new song follows a track by the artist it was found through"
    );
    let counted: i64 = rig
        .platform
        .db
        .read("test.reads", |conn| {
            conn.query_row(
                "SELECT COALESCE(SUM(reads), 0) FROM catalog_background_read_days \
                 WHERE source = 'listenbrainz'",
                [],
                |row| row.get(0),
            )
            .map_err(DbError::from)
        })
        .await
        .expect("reads counted");
    assert_eq!(counted, 2, "the songs and their albums, one read each");
    let musicbrainz: Vec<String> = wire
        .sent()
        .iter()
        .map(|sent| sent.url.to_string())
        .filter(|url| url.contains("/musicbrainz/"))
        .collect();
    assert!(
        musicbrainz.is_empty(),
        "no MusicBrainz read: {musicbrainz:?}"
    );
}

/// A mix as kept, with one track a row of `(id, kind, artist, release group)`,
/// naming the first two artists on it. A new track names no library track.
fn kept_mix(user: &str, name: &str, tracks: &[(&str, &str, usize, Option<String>)]) -> String {
    let mut named: Vec<serde_json::Value> = Vec::new();
    for (_, _, artist, _) in tracks {
        let artist =
            serde_json::json!({"artist_mbid": mbid(*artist), "name": format!("Artist {artist}")});
        if named.len() < 2 && !named.contains(&artist) {
            named.push(artist);
        }
    }
    let named = Value::Array(named).to_string();
    let mut sql = format!(
        "DELETE FROM catalog_mix_tracks WHERE mix_id = '{user}:0'; \
         DELETE FROM catalog_mixes WHERE id = '{user}:0'; \
         INSERT INTO catalog_mixes (id, user_id, mix_no, day, built_at, name, \
         group_anchor_mbid, named_artists, yours_count, new_count) VALUES ('{user}:0', \
         '{user}', 0, '2026-11-28', 1, '{name}', '{a}', '{named}', {n}, 0);",
        a = mbid(1),
        n = tracks.len(),
    );
    for (position, (id, kind, artist, group)) in tracks.iter().enumerate() {
        let group = group
            .as_ref()
            .map_or_else(|| "NULL".to_owned(), |group| format!("'{group}'"));
        sql.push_str(&format!(
            "INSERT INTO catalog_mix_tracks (mix_id, position, kind, local_track_id, \
             release_group_mbid, title, artist_name, artist_mbid, reason) VALUES ('{user}:0', \
             {position}, '{kind}', {track}, {group}, '{id}', 'Artist {artist}', '{m}', {reason});",
            m = mbid(*artist),
            track = if *kind == "new" {
                "NULL".to_owned()
            } else {
                format!("'{id}'")
            },
            reason = if *kind == "new" {
                "'{\"kind\":\"your_artists\",\"line\":\"Close to Artist 1.\",\"artists\":[]}'"
            } else {
                "NULL"
            },
        ));
    }
    sql
}

/// A hidden album's tracks and a hidden artist's never appear in a mix,
/// whatever put them there, the taste's own artists included, and the
/// mix's counts follow what is left.
#[tokio::test]
async fn a_mix_leaves_out_what_the_person_hid() {
    let rig = AuthRig::new("mixes-hidden").await;
    let mut tracks = Vec::new();
    for artist in [1, 2, 3, 4] {
        tracks.extend(by(artist, 2, None));
    }
    plant(&rig, tracks).await;
    let mut rows = kept_mix(
        "user-1",
        "Artist 1, Artist 2 and more",
        &[
            ("a1-t0", "artist", 1, None),
            ("a1-t1", "artist", 1, Some(group_of(9))),
            ("a2-t0", "artist", 2, None),
            ("a3-t0", "sound", 3, Some(group_of(9))),
            ("a4-t0", "sound", 4, None),
        ],
    );
    rows += &format!(
        "INSERT INTO catalog_discover_feedback (user_id, target, target_mbid_lower, kind, \
         created_at) VALUES ('user-1', 'album', '{album}', 'no', 1), \
         ('user-1', 'artist', '{artist}', 'not_artist', 1);",
        album = group_of(9),
        artist = mbid(2),
    );
    insert(&rig.platform.db, rows).await;
    let dir = crate::common::ScratchDir::new("mixes-hidden");
    let page = settled_page(&rig, &discover_routes(deps(&rig, &dir)), "the page").await;
    let section = page["sections"]
        .as_array()
        .expect("sections")
        .iter()
        .find(|section| section["key"] == "daily_mixes")
        .cloned()
        .expect("the mix is on the page");
    let shown: Vec<DailyMix> =
        serde_json::from_value(section["content"]["mixes"].clone()).expect("mixes read");
    let titles: Vec<&str> = shown[0]
        .tracks
        .iter()
        .map(|track| track.title.as_str())
        .collect();
    assert_eq!(titles, ["a1-t0", "a4-t0"]);
    assert_eq!((shown[0].yours, shown[0].new), (2, 0));
    assert_eq!(
        shown[0].name, "Artist 1 and more",
        "the hidden artist leaves the name"
    );
    let named: Vec<&str> = shown[0]
        .artists
        .iter()
        .map(|artist| artist.name.as_str())
        .collect();
    assert_eq!(named, ["Artist 1"]);
}

/// A person seen lately with a taste build has their mixes queued once a
/// UTC day by the clock, someone with no taste build never, and a visit
/// queues them while the warm cycle is off.
#[tokio::test]
async fn mixes_are_queued_once_a_day() {
    use crate::discover_week::{due, queued, seen_at};
    use droppedneedle_catalog::discover::mixes::KIND;
    use droppedneedle_kernel::clock::Clock as _;

    let rig = AuthRig::new("mixes-due").await;
    plant(&rig, Vec::new()).await;
    insert(&rig.platform.db, taste("user-1", &[(1.0, &[(1, 9)])])).await;
    let now = rig.clock().now_ms();
    seen_at(&rig.platform.db, "user-1", now).await;
    seen_at(&rig.platform.db, "user-2", now).await;
    let db = rig.platform.db.clone();
    let kind = KIND.name();

    let clock = due(&rig, true);
    clock.clock_pass().await;
    assert!(
        queued(&db, kind, "user-1"),
        "the clock queues the day's mixes"
    );
    clock.clock_pass().await;
    assert!(!queued(&db, kind, "user-2"), "no taste build, no mixes");
    assert!(!queued(&db, kind, "user-1"), "not again the same day");
    // Their first pool lands: once more that day, so the mixes take it in.
    insert(
        &db,
        "INSERT INTO catalog_discover_pool_builds (user_id, built_at, incomplete, \
         waiting_for, seeds_used, neighbours_found, known_left_out, hubs_left_out, \
         artists_ranked, artists_without_album, new_artist_candidates, \
         source_pick_candidates, gap_candidates, shelf_candidates, fresh_candidates, \
         thread_candidates) VALUES ('user-1', 2, 0, '[]', 1, 1, 0, NULL, 1, 0, 0, 0, 0, \
         0, 0, 0);"
            .to_owned(),
    )
    .await;
    clock.clock_pass().await;
    assert!(
        queued(&db, kind, "user-1"),
        "the first pool queues them once more"
    );
    clock.clock_pass().await;
    assert!(!queued(&db, kind, "user-1"), "and only once");
    rig.clock().advance(20 * 60_000);
    clock.clock_pass().await;
    assert!(!queued(&db, kind, "user-1"), "once a day");
    rig.clock().advance(86_400_000);
    clock.clock_pass().await;
    assert!(queued(&db, kind, "user-1"), "and again the next day");

    let off = due(&rig, false);
    off.clock_pass().await;
    assert!(
        !queued(&db, kind, "user-1"),
        "the clock queues nothing while off"
    );
    off.visit("user-1").await;
    assert!(queued(&db, kind, "user-1"), "a visit queues them");
}

/// Plays of `tracks` by `user` a day ago.
fn played(rig: &AuthRig, user: &str, tracks: &[String]) -> String {
    use droppedneedle_kernel::clock::Clock as _;
    let at = rig.clock().now_ms() - 86_400_000;
    tracks
        .iter()
        .map(|track| {
            format!(
                "INSERT INTO library_play_history (id, user_id, local_track_id, track_name, \
                 artist_name, played_at) VALUES ('{user}-{track}', '{user}', '{track}', \
                 'Song', 'Someone', {at});"
            )
        })
        .collect()
}

/// A person's own plays bring their played tracks into their mix, where
/// there are more tracks than the mix takes; another person's plays of the
/// same tracks change nothing in it.
#[tokio::test]
async fn only_a_persons_own_plays_shape_their_mix() {
    let rig = AuthRig::new("mixes-plays").await;
    let mut tracks = Vec::new();
    for artist in 1..=20 {
        tracks.extend(by(artist, 4, None));
    }
    plant(&rig, tracks).await;
    let artists: Vec<(usize, i64)> = (1..=20).map(|artist| (artist, 1)).collect();
    insert(&rig.platform.db, taste("user-1", &[(1.0, &artists)])).await;
    sound_on(&rig, false);
    let dir = crate::common::ScratchDir::new("mixes-plays");
    let deps = deps(&rig, &dir);
    let late: Vec<String> = (7..=9)
        .flat_map(|artist| (0..4).map(move |n| format!("a{artist}-t{n}")))
        .collect();
    let has_late = |mix: &DailyMix| mix.tracks.iter().any(|track| late.contains(&track.title));
    build(&deps, "user-1").await;
    let before = mixes(&rig, "user-1").await;

    insert(&rig.platform.db, played(&rig, "user-2", &late)).await;
    build(&deps, "user-1").await;
    assert_eq!(
        mixes(&rig, "user-1").await,
        before,
        "someone else's plays change nothing"
    );

    insert(&rig.platform.db, played(&rig, "user-1", &late)).await;
    build(&deps, "user-1").await;
    let after = mixes(&rig, "user-1").await;
    assert!(has_late(&after[0]), "their own plays do: {:?}", after[0]);
    assert_ne!(after, before);
}

/// Start radio on a mix plays a station grown from the mix's own library
/// tracks, as a playlist's does; another person's mix is not found.
#[tokio::test]
async fn a_station_grows_from_a_mix() {
    let rig = AuthRig::new("mixes-radio").await;
    let mut tracks = Vec::new();
    for artist in 1..=6 {
        tracks.extend(by(artist, 2, Some(artist as f64 * 3.0)));
    }
    plant(&rig, tracks).await;
    insert(
        &rig.platform.db,
        kept_mix(
            "user-1",
            "Artist 1, Artist 2 and more",
            &[
                ("a1-t0", "artist", 1, None),
                ("a2-t0", "artist", 2, None),
                ("a3-t0", "sound", 3, None),
            ],
        ) + &kept_mix("user-2", "Artist 4", &[("a4-t0", "artist", 4, None)]),
    )
    .await;
    sound_on(&rig, true);
    let dir = crate::common::ScratchDir::new("mixes-radio");
    let app = discover_routes(deps(&rig, &dir));
    let ask = |seed: &str| serde_json::json!({"seed_type": "playlist", "seed_id": seed, "mode": "library", "count": 5});
    let (status, plan) = crate::discover_queue::call(
        &app,
        "POST",
        "/api/v3/discover/radio/plan",
        Some(ask("user-1:0")),
    )
    .await;
    assert_eq!(status, 200, "{plan}");
    assert_eq!(plan["title"], "Radio: Artist 1, Artist 2 and more");
    let tracks = plan["tracks"].as_array().expect("tracks");
    assert!(!tracks.is_empty(), "{plan}");
    assert!(tracks.iter().all(|track| track["in_library"] == true));
    let (status, _) = crate::discover_queue::call(
        &app,
        "POST",
        "/api/v3/discover/radio/plan",
        Some(ask("user-2:0")),
    )
    .await;
    assert_eq!(status, 404, "another person's mix is not theirs to play");
}

/// A stored new track whose album someone asked for since is served as
/// requested, so Keep and Download all leave it out.
#[tokio::test]
async fn a_new_track_asked_for_since_is_served_requested() {
    let rig = AuthRig::new("mixes-requested").await;
    plant(&rig, by(1, 2, None)).await;
    let mut rows = kept_mix(
        "user-1",
        "Artist 1 and more",
        &[
            ("a1-t0", "artist", 1, None),
            ("Fresh", "new", 20, Some(group_of(1))),
        ],
    );
    rows += &format!(
        "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
         album_title, requested_at, status, user_id) VALUES ('{g}', '{g}', 'Newcomer', \
         'Album 1', 1, 'pending', 'user-2');",
        g = group_of(1),
    );
    insert(&rig.platform.db, rows).await;
    let dir = crate::common::ScratchDir::new("mixes-requested");
    let page = settled_page(&rig, &discover_routes(deps(&rig, &dir)), "the page").await;
    let section = page["sections"]
        .as_array()
        .expect("sections")
        .iter()
        .find(|section| section["key"] == "daily_mixes")
        .cloned()
        .expect("the mix is on the page");
    let shown: Vec<DailyMix> =
        serde_json::from_value(section["content"]["mixes"].clone()).expect("mixes read");
    let fresh = shown[0]
        .tracks
        .iter()
        .find(|track| track.kind == MixTrackKind::New)
        .expect("the new track");
    assert!(fresh.album.requested, "{fresh:?}");
    assert!(!fresh.album.in_library);
}

/// With no ListenBrainz link, a mix takes no new tracks and sends
/// nothing anywhere for them, YouTube and a Last.fm key set up or not.
#[tokio::test]
async fn an_unlinked_person_gets_no_new_tracks_and_sends_nothing() {
    use droppedneedle_kernel::runtime_config::Secret;
    use droppedneedle_kernel::runtime_config::secret_sections::YouTubeConnection;
    use droppedneedle_providers::test_support::ScriptedWire;

    let rig = AuthRig::new("mixes-unlinked").await;
    let mut tracks = Vec::new();
    for artist in 1..=4 {
        tracks.extend(by(artist, 3, None));
    }
    plant(&rig, tracks).await;
    let mut rows = taste("user-1", &[(1.0, &[(1, 9), (2, 8), (3, 7), (4, 6)])]);
    rows += "INSERT INTO catalog_discover_pool_builds (user_id, built_at, incomplete, \
         waiting_for, seeds_used, neighbours_found, known_left_out, hubs_left_out, \
         artists_ranked, artists_without_album, new_artist_candidates, source_pick_candidates, \
         gap_candidates, shelf_candidates, fresh_candidates, thread_candidates) VALUES \
         ('user-1', 2, 0, '[]', 1, 1, 0, NULL, 1, 0, 1, 0, 0, 0, 0, 0);";
    rows += &candidate("user-1", 0, 1);
    insert(&rig.platform.db, rows).await;
    let sealed = rig.deps.crypto.encrypt("test-lastfm-key").expect("sealed");
    rig.deps
        .lastfm
        .upsert(
            "user-1",
            droppedneedle_auth::users::models::LastFmConnection {
                configured: true,
                api_key_encrypted: Some(sealed),
                ..Default::default()
            },
        )
        .await
        .expect("Last.fm key");
    crate::discover_queue::seen_now(&rig, "user-1").await;
    rig.config
        .save_secret_blocking(YouTubeConnection {
            api_key: Secret::new("youtube-key"),
            enabled: true,
            api_enabled: true,
            daily_quota_limit: 10_000,
        })
        .expect("youtube settings save");
    sound_on(&rig, false);
    let wire = Arc::new(ScriptedWire::new());
    let dir = crate::common::ScratchDir::new("mixes-unlinked");
    let deps = discover_deps(&rig, &dir, Providers::unpaced(wire.clone()), |_| {});
    build(&deps, "user-1").await;

    let mix = mixes(&rig, "user-1").await.remove(0);
    assert_eq!(mix.new, 0, "{mix:?}");
    let sent: Vec<String> = wire
        .sent()
        .iter()
        .map(|sent| sent.url.to_string())
        .collect();
    assert!(sent.is_empty(), "nothing sent: {sent:?}");
    let counted: i64 = rig
        .platform
        .db
        .read("test.reads", |conn| {
            conn.query_row(
                "SELECT COALESCE(SUM(reads), 0) FROM catalog_background_read_days",
                [],
                |row| row.get(0),
            )
            .map_err(DbError::from)
        })
        .await
        .expect("reads counted");
    assert_eq!(counted, 0);
}

/// `user`'s app playlists from Discover: each one's name and its library
/// tracks in order.
async fn app_lists(rig: &AuthRig, user: &str) -> Vec<(String, Vec<String>)> {
    let user = user.to_owned();
    rig.platform
        .db
        .read("test.app_lists", move |conn| {
            let lists: Vec<(String, String)> = conn
                .prepare(
                    "SELECT id, name FROM catalog_playlists WHERE user_id = ?1 \
                     AND source_ref LIKE 'discover-list:mix:%' ORDER BY source_ref",
                )?
                .query_map([&user], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let mut out = Vec::new();
            for (id, name) in lists {
                let tracks = conn
                    .prepare(
                        "SELECT library_file_id FROM catalog_playlist_tracks \
                         WHERE playlist_id = ?1 ORDER BY position",
                    )?
                    .query_map([&id], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                out.push((name, tracks));
            }
            Ok::<_, DbError>(out)
        })
        .await
        .expect("app playlists read")
}

/// A kept Daily Mix reaches its owner's apps as a playlist named as the
/// mix, its library tracks in the mix's order and its new tracks left out;
/// nobody else gets it, and with the person's switch off it is gone.
#[tokio::test]
async fn a_kept_mix_is_a_playlist_in_its_owners_apps() {
    use droppedneedle_catalog::discover::engine::weekly::{Depth, SetDay};
    use droppedneedle_catalog::discover::models::DiscoverPrefs;

    let rig = AuthRig::new("mixes-app-lists").await;
    let mut tracks = Vec::new();
    for artist in [1, 2] {
        tracks.extend(by(artist, 2, None));
    }
    plant(&rig, tracks).await;
    insert(
        &rig.platform.db,
        kept_mix(
            "user-1",
            "Artist 1, Artist 2 and more",
            &[
                ("a2-t1", "artist", 2, None),
                ("Fresh one", "new", 3, Some(group_of(9))),
                ("a1-t0", "artist", 1, None),
                ("a2-t0", "sound", 2, None),
            ],
        ),
    )
    .await;
    let dir = crate::common::ScratchDir::new("mixes-app-lists");
    let deps = deps(&rig, &dir);
    for user in ["user-1", "user-2"] {
        deps.app_lists
            .refresh(user, |_| Ok(()))
            .await
            .expect("app playlists refreshed");
    }
    assert_eq!(
        app_lists(&rig, "user-1").await,
        [(
            "Artist 1, Artist 2 and more".to_owned(),
            vec!["a2-t1".to_owned(), "a1-t0".to_owned(), "a2-t0".to_owned()]
        )]
    );
    assert!(
        app_lists(&rig, "user-2").await.is_empty(),
        "only its owner's"
    );

    deps.weekly
        .save_prefs(
            "user-1",
            DiscoverPrefs {
                set_day: SetDay::Friday,
                depth: Depth::NextDoor,
                app_playlists: false,
            },
        )
        .await
        .expect("the switch is saved");
    deps.app_lists
        .refresh("user-1", |_| Ok(()))
        .await
        .expect("app playlists refreshed");
    assert!(
        app_lists(&rig, "user-1").await.is_empty(),
        "the switch off takes it away"
    );
}

/// A refresh that read the person's mixes before they forgot their history
/// keeps nothing once the forget has run: the forgotten mix does not come
/// back as a playlist.
#[tokio::test]
async fn an_app_playlists_refresh_across_a_forget_keeps_nothing() {
    let rig = AuthRig::new("mixes-app-lists-forget").await;
    plant(&rig, by(1, 2, None)).await;
    insert(
        &rig.platform.db,
        kept_mix(
            "user-1",
            "Artist 1 and more",
            &[("a1-t0", "artist", 1, None), ("a1-t1", "artist", 1, None)],
        ),
    )
    .await;
    let dir = crate::common::ScratchDir::new("mixes-app-lists-forget");
    let deps = deps(&rig, &dir);
    let began = deps.app_lists.forgets_now("user-1");
    droppedneedle_catalog::discover::forget::forget(&deps, "user-1")
        .await
        .expect("forgotten");
    // The mix the refresh read is put back, so only the forget count keeps
    // the refresh from writing it.
    insert(
        &rig.platform.db,
        kept_mix(
            "user-1",
            "Artist 1 and more",
            &[("a1-t0", "artist", 1, None), ("a1-t1", "artist", 1, None)],
        ),
    )
    .await;
    deps.app_lists
        .refresh_from("user-1", began, |_| Ok(()))
        .await
        .expect("the refresh ends");
    assert!(
        app_lists(&rig, "user-1").await.is_empty(),
        "a refresh claimed before the forget kept nothing"
    );
    deps.app_lists
        .refresh("user-1", |_| Ok(()))
        .await
        .expect("app playlists refreshed");
    assert_eq!(
        app_lists(&rig, "user-1").await.len(),
        1,
        "a later one keeps"
    );
}

/// A list is made only once it has songs: someone with no set, shelves or
/// mix tracks gets none. One whose songs all go stays, empty, under the
/// same id, so an app that pinned it keeps it.
#[tokio::test]
async fn an_app_playlist_is_made_with_songs_and_kept_when_it_empties() {
    let rig = AuthRig::new("mixes-app-lists-empty").await;
    plant(&rig, by(1, 1, None)).await;
    insert(
        &rig.platform.db,
        kept_mix(
            "user-1",
            "Artist 1 and more",
            &[("a1-t0", "artist", 1, None)],
        ),
    )
    .await;
    let dir = crate::common::ScratchDir::new("mixes-app-lists-empty");
    let deps = deps(&rig, &dir);
    let lists = |user: &'static str| {
        let db = rig.platform.db.clone();
        async move {
            db.read("test.discover_lists", move |conn| {
                conn.prepare(
                    "SELECT id FROM catalog_playlists WHERE user_id = ?1 \
                     AND source_ref LIKE 'discover-list:%' ORDER BY id",
                )?
                .query_map([user], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(DbError::from)
            })
            .await
            .expect("lists read")
        }
    };
    for user in ["user-1", "user-2"] {
        deps.app_lists
            .refresh(user, |_| Ok(()))
            .await
            .expect("app playlists refreshed");
    }
    assert!(lists("user-2").await.is_empty(), "nothing to play, no list");
    let made = lists("user-1").await;
    assert_eq!(made.len(), 1, "only the mix has songs");

    // Their one artist hidden from Discover leaves the mix with no song.
    insert(
        &rig.platform.db,
        format!(
            "INSERT INTO catalog_discover_feedback (user_id, target, target_mbid_lower, kind, \
             created_at) VALUES ('user-1', 'artist', '{}', 'not_artist', 1);",
            mbid(1)
        ),
    )
    .await;
    deps.app_lists
        .refresh("user-1", |_| Ok(()))
        .await
        .expect("app playlists refreshed");
    assert_eq!(lists("user-1").await, made, "the emptied list stays");
    assert!(
        app_lists(&rig, "user-1").await[0].1.is_empty(),
        "and holds no song"
    );
}
