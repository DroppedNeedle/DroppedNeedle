//! The section registry: every settings section served as itself by a
//! get/put pair, listed once. A row names the section, its path, who may
//! read it and who may save it, and each operation's document text; the
//! routes and their document entries come from the row. Save effects are
//! declared on the section, and the stored keys come from the kernel's
//! section list. Sections with their own wire shapes or checks keep
//! hand-written routes in [`handlers`](super::handlers).
//!
//! A plain section crosses the wire as itself, a secret section as
//! [`Masked`], so a decrypted value cannot be returned and a masked secret
//! sent back keeps the stored one.

use axum::{Json, extract::State};

use super::wiring::SettingsSetup;
use droppedneedle_auth::{Admin, User};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::ValidJson;
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_kernel::runtime_config::secret_sections::{
    AdvancedSettings, EventsSettings, JellyfinConnection, LastFmSettings, ListenBrainzConnection,
    NavidromeConnection, OidcConnection, PlexConnection, ProwlarrConnection, SlskdConnection,
    WrappedSettings, YouTubeConnection,
};
use droppedneedle_kernel::runtime_config::sections::{
    ConnectApps, EditionPreferences, FilesystemWatcher, GetIt, LyricsSettings, PlainSection,
    PrimaryMusicSource, PublicAddress, SecuritySettings, UserPreferences, WantedWatcher,
};
use droppedneedle_kernel::runtime_config::{Masked, SecretSection, Section};

type JsonResult<T> = Result<Json<T>, ApiError>;

/// Read a plain section.
fn read_plain<S: PlainSection>(settings: &SettingsSetup) -> JsonResult<S> {
    settings.service().get().map(Json).map_err(ApiError::from)
}

/// Validate and save a plain section, then run its effects.
async fn save_plain<S: PlainSection + Send + 'static>(
    settings: &SettingsSetup,
    body: S,
) -> JsonResult<S> {
    settings
        .service()
        .save(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Read a secret section with its secrets masked.
fn read_secret<S: SecretSection>(settings: &SettingsSetup) -> JsonResult<Masked<S>> {
    settings
        .service()
        .get_masked()
        .map(Json)
        .map_err(ApiError::from)
}

/// Save a secret section (a masked secret keeps the stored one), then run
/// its effects.
async fn save_secret<S: SecretSection + Send + 'static>(
    settings: &SettingsSetup,
    body: Masked<S>,
) -> JsonResult<Masked<S>> {
    settings
        .service()
        .save_masked(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Expands each row into an annotated get and put handler at its path,
/// [`routes`], which mounts each in the group the row names, and
/// [`ROUTED_KEYS`]. An operation id is its handler's name; summaries and
/// 200 descriptions are the row's text, and a put may list more responses.
/// utoipa annotations are static, so each row gets its own handlers over
/// the generic reads and saves above.
macro_rules! section_routes {
    ($(
        $kind:ident $ty:ident at $path:tt, read $read:ident, write $write:ident {
            get $get:ident: $get_summary:tt => $get_description:tt;
            put $put:ident: $put_summary:tt => $put_description:tt $(, $extra:tt)*;
        }
    )*) => {
        $(
            #[utoipa::path(
                get,
                tag = "settings",
                path = $path,
                summary = $get_summary,
                responses((status = 200, description = $get_description, body = $ty))
            )]
            async fn $get(
                State(settings): State<SettingsSetup>,
                _caller: section_routes!(@caller $read),
            ) -> JsonResult<section_routes!(@wire $kind $ty)> {
                section_routes!(@read $kind settings)
            }

            #[utoipa::path(
                put,
                tag = "settings",
                path = $path,
                summary = $put_summary,
                request_body = $ty,
                responses(
                    (status = 200, description = $put_description, body = $ty)
                    $(, $extra)*
                )
            )]
            async fn $put(
                State(settings): State<SettingsSetup>,
                _caller: section_routes!(@caller $write),
                ValidJson(body): ValidJson<section_routes!(@wire $kind $ty)>,
            ) -> JsonResult<section_routes!(@wire $kind $ty)> {
                section_routes!(@save $kind settings body)
            }
        )*

        /// Every registry route, each in the group its row names.
        pub fn routes() -> Routes<SettingsSetup> {
            Routes::new()
                $(
                    .$read(routes!($get))
                    .$write(routes!($put))
                )*
        }

        /// The key of every section the registry serves.
        pub const ROUTED_KEYS: &[&str] = &[$(<$ty as Section>::KEY,)*];
    };
    (@caller admin) => { Admin };
    (@caller user) => { User };
    (@wire plain $ty:ident) => { $ty };
    (@wire secret $ty:ident) => { Masked<$ty> };
    (@read plain $settings:ident) => { read_plain(&$settings) };
    (@read secret $settings:ident) => { read_secret(&$settings) };
    (@save plain $settings:ident $body:ident) => { save_plain(&$settings, $body).await };
    (@save secret $settings:ident $body:ident) => { save_secret(&$settings, $body).await };
}

section_routes! {
    plain UserPreferences at "/api/v3/settings/preferences", read admin, write admin {
        get get_preferences: "Read the release-type filters."
            => "Release-type filters";
        put put_preferences: "Save the release-type filters."
            => "Saved filters";
    }
    plain FilesystemWatcher at "/api/v3/settings/library/watcher", read admin, write admin {
        get get_watcher: "Read the filesystem poller knobs."
            => "Filesystem poller knobs";
        put put_watcher: "Save the filesystem poller knobs."
            => "Saved poller knobs";
    }
    plain LyricsSettings at "/api/v3/settings/lyrics", read admin, write admin {
        get get_lyrics_settings: "Read the online lyrics lookup switch."
            => "Lyrics lookup switch";
        put put_lyrics_settings: "Save the online lyrics lookup switch."
            => "Saved switch";
    }
    plain PrimaryMusicSource at "/api/v3/settings/primary-source", read admin, write admin {
        get get_primary_source: "Read the primary music source."
            => "Primary music source";
        put put_primary_source: "Save the primary music source."
            => "Saved source";
    }
    plain PublicAddress at "/api/v3/settings/public-address", read admin, write admin {
        get get_public_address: "Read the addresses stated as this server's own."
            => "Stated public addresses";
        put put_public_address: "Replace the addresses stated as this server's own. The session origin\ncheck accepts a change from a page at one of them."
            => "The list as stored",
            (status = 400, description = "An entry is not an address, or there are more than eight");
    }
    plain SecuritySettings at "/api/v3/settings/security", read admin, write admin {
        get get_security: "Read the security posture settings."
            => "Security settings";
        put put_security: "Save the security posture settings."
            => "Saved settings";
    }
    // Any signed-in user reads it: the Profile page shows which protocols
    // are on. Nothing in it is secret.
    plain ConnectApps at "/api/v3/settings/connect-apps", read user, write admin {
        get get_connect_apps: "Read the inbound Connect Apps config."
            => "Connect Apps config";
        put put_connect_apps: "Save the inbound Connect Apps config."
            => "Saved config";
    }
    plain GetIt at "/api/v3/settings/get-it", read admin, write admin {
        get get_get_it: "Read the store-region settings."
            => "Store-region settings";
        put put_get_it: "Save the store-region settings."
            => "Saved settings";
    }
    plain EditionPreferences at "/api/v3/settings/edition-preferences", read admin, write admin {
        get get_edition_preferences: "Read the edition preferences."
            => "Edition preferences";
        put put_edition_preferences: "Save the edition preferences."
            => "Saved settings";
    }
    plain WantedWatcher at "/api/v3/settings/download-clients/wanted", read admin, write admin {
        get get_wanted: "Read the wanted watcher toggles."
            => "Wanted watcher toggles";
        put put_wanted: "Save the wanted watcher toggles."
            => "Saved toggles";
    }
    secret AdvancedSettings at "/api/v3/settings/advanced", read admin, write admin {
        get get_advanced: "Read the advanced tunables (AudioDB key masked unless unset)."
            => "Advanced tunables";
        put put_advanced: "Save the advanced tunables (a masked AudioDB key keeps the stored one,\na blank one stores the free default)."
            => "Saved tunables";
    }
    secret SlskdConnection at "/api/v3/settings/download-client/config", read admin, write admin {
        get get_slskd: "Read the slskd connection (key masked unless unset)."
            => "slskd connection";
        put put_slskd: "Save the slskd connection (a masked key keeps the stored one)."
            => "Saved connection";
    }
    secret EventsSettings at "/api/v3/settings/events", read admin, write admin {
        get get_events: "Read the events sources (keys masked unless unset)."
            => "Events sources";
        put put_events: "Save the events sources (masked keys keep the stored ones) and kick\nthe sweep."
            => "Saved sources";
    }
    secret JellyfinConnection at "/api/v3/settings/jellyfin", read admin, write admin {
        get get_jellyfin: "Read the Jellyfin connection (key masked unless unset)."
            => "Jellyfin connection";
        put put_jellyfin: "Save the Jellyfin connection (a masked key keeps the stored one)."
            => "Saved connection";
    }
    secret LastFmSettings at "/api/v3/settings/lastfm", read admin, write admin {
        get get_lastfm: "Read the Last.fm switch and the instance app key pair (masked unless\nunset)."
            => "Last.fm settings";
        put put_lastfm: "Save the Last.fm switch and the instance app key pair (a masked value\nkeeps the stored one)."
            => "Saved settings";
    }
    secret ListenBrainzConnection at "/api/v3/settings/listenbrainz", read admin, write admin {
        get get_listenbrainz: "Read the ListenBrainz connection (token masked unless unset)."
            => "ListenBrainz connection";
        put put_listenbrainz: "Save the ListenBrainz connection (a masked token keeps the stored one)."
            => "Saved connection";
    }
    secret NavidromeConnection at "/api/v3/settings/navidrome", read admin, write admin {
        get get_navidrome: "Read the Navidrome connection (password masked unless unset)."
            => "Navidrome connection";
        put put_navidrome: "Save the Navidrome connection (a masked password keeps the stored one)."
            => "Saved connection";
    }
    secret OidcConnection at "/api/v3/settings/oidc", read admin, write admin {
        get get_oidc: "Read the OIDC connection (secret masked unless unset)."
            => "OIDC connection";
        put put_oidc: "Save the OIDC connection (a masked secret keeps the stored one)."
            => "Saved connection";
    }
    secret PlexConnection at "/api/v3/settings/plex", read admin, write admin {
        get get_plex: "Read the Plex connection (token masked unless unset)."
            => "Plex connection";
        put put_plex: "Save the Plex connection (a masked token keeps the stored one)."
            => "Saved connection";
    }
    secret ProwlarrConnection at "/api/v3/settings/prowlarr/config", read admin, write admin {
        get get_prowlarr: "Read the Prowlarr connection (key masked unless unset)."
            => "Prowlarr connection";
        put put_prowlarr: "Save the Prowlarr connection (a masked key keeps the stored one)."
            => "Saved connection";
    }
    secret WrappedSettings at "/api/v3/settings/wrapped", read admin, write admin {
        get get_wrapped: "Read the wrapped settings (key masked unless unset)."
            => "Wrapped settings";
        put put_wrapped: "Save the wrapped settings (a masked key keeps the stored one)."
            => "Saved settings";
    }
    secret YouTubeConnection at "/api/v3/settings/youtube", read admin, write admin {
        get get_youtube: "Read the YouTube connection (key masked unless unset)."
            => "YouTube connection";
        put put_youtube: "Save the YouTube connection (a masked key keeps the stored one)."
            => "Saved connection";
    }
}

#[cfg(test)]
mod tests {
    use super::ROUTED_KEYS;
    use droppedneedle_kernel::runtime_config::registry::SECTION_KEYS;

    /// Sections on the kernel's list that the registry does not serve, and
    /// what serves them instead.
    const ELSEWHERE: &[(&str, &str)] = &[
        (
            "library_scan_schedule",
            "the schedule routes add the last scan",
        ),
        ("library_settings", "the library routes check a revision"),
        ("download_policy", "the policy routes send a view"),
        ("source_priority", "the source order routes"),
        ("usenet_search_backend", "the search backend routes"),
        ("download_clients", "the SABnzbd routes"),
        ("musicbrainz_settings", "the MusicBrainz lifecycle routes"),
        ("library_management", "the Library Management routes"),
        ("spotify_settings", "the Spotify import routes"),
        ("lidarr_import", "the Lidarr import routes"),
        ("indexers", "the indexer routes"),
        ("plugins", "the plugin routes"),
        ("_internal", "server state, never edited"),
    ];

    /// Every section on the kernel's list is served by the registry or
    /// named above, never both, so a new section needs a decision.
    #[test]
    fn every_listed_section_has_its_routes() {
        for key in SECTION_KEYS {
            let routed = ROUTED_KEYS.contains(key);
            let elsewhere = ELSEWHERE.iter().any(|(other, _)| other == key);
            assert!(
                routed != elsewhere,
                "{key}: routed {routed}, elsewhere {elsewhere}"
            );
        }
        for (key, _) in ELSEWHERE {
            assert!(
                SECTION_KEYS.contains(key),
                "{key} is not on the section list"
            );
        }
    }
}
