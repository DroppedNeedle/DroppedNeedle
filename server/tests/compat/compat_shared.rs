//! Shared compat security posture: the public Subsonic endpoint, query
//! redaction, the rate-limit and auth-backoff budgets, the address
//! player apps are given and the signature on open picture links. The
//! credential schemes are pinned in `auth_compat_goldens`.

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use droppedneedle_compat::settings::LiveSettings;
use droppedneedle_compat::shared::address::{ListenerConfig, base_address};
use droppedneedle_compat::shared::{auth, ratelimit, redact};
use droppedneedle_compat::subsonic::picture::{PURPOSE, PictureKind, PictureSigner};
use droppedneedle_kernel::config::TrustedProxies;
use droppedneedle_kernel::runtime_config::Crypto;
use droppedneedle_kernel::runtime_config::sections::PublicAddress;

/// One Subsonic endpoint skips auth, in any case and with `.view`. The
/// Jellyfin public routes are walked through the app in
/// `compat_jellyfin_library`.
#[test]
fn subsonic_public_endpoint() {
    assert!(auth::subsonic_is_public("getopensubsonicextensions.view"));
    assert!(!auth::subsonic_is_public("stream"));
}

/// Credentials ride in compat query strings; anything logged goes through
/// this redactor, which masks every spelling of every secret key.
#[test]
fn redaction_masks_query_secrets() {
    for (target, expected) in [
        (
            "/subsonic/rest/ping?u=ada&p=hunter2&t=tok&s=salt",
            "/subsonic/rest/ping?u=ada&p=***&t=***&s=***",
        ),
        (
            "/jellyfin/Audio/x/stream?api_key=secret&static=true",
            "/jellyfin/Audio/x/stream?api_key=***&static=true",
        ),
        (
            "/subsonic/rest/stream?transcodeParams=blob&id=tr-1",
            "/subsonic/rest/stream?transcodeParams=***&id=tr-1",
        ),
        (
            "/subsonic/rest/ping?P=A&T=B&ApiKey=C&TOKEN=D&PW=E&PASSWORD=F",
            "/subsonic/rest/ping?P=***&T=***&ApiKey=***&TOKEN=***&PW=***&PASSWORD=***",
        ),
        // Percent-encoded keys are decoded before matching.
        (
            "/subsonic/rest/ping?%70=%68i&id=%70",
            "/subsonic/rest/ping?p=***&id=p",
        ),
        (
            "/subsonic/rest/ping?id=1&id=2&p=x",
            "/subsonic/rest/ping?id=1&id=2&p=***",
        ),
        ("/subsonic/rest/ping", "/subsonic/rest/ping"),
        // The signature of an open picture link.
        (
            "/subsonic/img?id=al-1&sig=abc123&size=500",
            "/subsonic/img?id=al-1&sig=***&size=500",
        ),
    ] {
        assert_eq!(redact::redact_request_target(target), expected);
    }
}

#[test]
fn rate_limit_budgets() {
    let mut limits = ratelimit::CompatRateLimits::new();
    // Public endpoints: a burst of 20 per IP, then a Retry-After.
    for _ in 0..20 {
        assert_eq!(limits.public_retry_after("10.0.0.1", 0.0), None);
    }
    assert!(
        limits
            .public_retry_after("10.0.0.1", 0.0)
            .is_some_and(|secs| secs >= 1)
    );
    assert_eq!(limits.public_retry_after("10.0.0.2", 0.0), None);
    // Per principal: 120 browse calls, 20 mutations.
    for _ in 0..120 {
        assert_eq!(limits.principal_retry_after("user:1", false, 0.0), None);
    }
    assert!(limits.principal_retry_after("user:1", false, 0.0).is_some());
    for _ in 0..20 {
        assert_eq!(limits.principal_retry_after("user:1", true, 0.0), None);
    }
    assert!(limits.principal_retry_after("user:1", true, 0.0).is_some());

    // Media is exempt from the buckets; logins and PlaybackInfo are not
    // mutations.
    assert!(ratelimit::is_media_request("/subsonic/rest/stream.view"));
    assert!(ratelimit::is_media_request("/jellyfin/audio/abc/stream"));
    assert!(ratelimit::is_media_request("/jellyfin/Items/abc/File"));
    assert!(ratelimit::is_media_request("/jellyfin/Items/abc/Download"));
    assert!(!ratelimit::is_media_request("/subsonic/rest/getCoverArt"));
    assert!(ratelimit::is_artwork_request(
        "/subsonic/rest/getCoverArt.view"
    ));
    assert!(ratelimit::is_artwork_request(
        "/jellyfin/Items/abc/Images/Primary/0"
    ));
    assert!(!ratelimit::is_artwork_request("/jellyfin/Items/abc"));
    assert!(ratelimit::is_mutation_request(
        "POST",
        "/subsonic/rest/star"
    ));
    assert!(ratelimit::is_mutation_request("DELETE", "/jellyfin/x"));
    assert!(!ratelimit::is_mutation_request(
        "POST",
        "/jellyfin/Users/AuthenticateByName"
    ));
    assert!(!ratelimit::is_mutation_request(
        "POST",
        "/jellyfin/Items/abc/PlaybackInfo"
    ));
}

/// Five failures inside the window lock the IP out for 10 seconds; the next
/// lockout doubles.
#[test]
fn auth_backoff_locks_out_then_doubles() {
    let mut limits = ratelimit::CompatRateLimits::new();
    for second in 0..4 {
        assert_eq!(
            auth::record_auth_denial(&mut limits, "10.0.0.9", f64::from(second)),
            None
        );
    }
    assert_eq!(
        auth::record_auth_denial(&mut limits, "10.0.0.9", 4.0),
        Some(10)
    );
    assert_eq!(auth::auth_locked_out(&mut limits, "10.0.0.9", 5.0), Some(9));
    assert_eq!(auth::auth_locked_out(&mut limits, "10.0.0.9", 15.0), None);
    for second in 20..24 {
        auth::record_auth_denial(&mut limits, "10.0.0.9", f64::from(second));
    }
    assert_eq!(
        auth::record_auth_denial(&mut limits, "10.0.0.9", 24.0),
        Some(20)
    );
}

/// The address an app is given: the host and scheme a trusted proxy
/// forwards, never those of an untrusted peer; a plain `http` address
/// becomes the one stated `https` address on its host name, and only when
/// there is exactly one; then the base path.
#[test]
fn base_address_for_player_apps() {
    struct Case {
        name: &'static str,
        peer: &'static str,
        forwarded: bool,
        stated: &'static [&'static str],
        base_path: &'static str,
        expected: &'static str,
    }
    const LAN: &str = "192.168.1.5:40000";
    let plain = Case {
        name: "no proxy, nothing stated",
        peer: LAN,
        forwarded: false,
        stated: &[],
        base_path: "",
        expected: "http://music.lan:8688",
    };
    let dir = crate::common::ScratchDir::new("base-address");
    let store = crate::common::config_store(&dir);
    let settings = LiveSettings::from_config(&store, false);
    let cases = [
        Case {
            name: "trusted proxy",
            peer: "127.0.0.1:40000",
            forwarded: true,
            expected: "https://music.example.com",
            ..plain
        },
        Case {
            name: "untrusted peer",
            peer: "203.0.113.9:40000",
            forwarded: true,
            ..plain
        },
        Case {
            name: "one https address stated on the host name",
            stated: &["https://music.lan", "http://192.168.1.5:8688"],
            expected: "https://music.lan",
            ..plain
        },
        Case {
            name: "two https addresses stated on the host name",
            stated: &["https://music.lan", "https://music.lan:8443"],
            ..plain
        },
        Case {
            name: "https address stated on another host name",
            stated: &["https://music.example.com"],
            ..plain
        },
        Case {
            name: "base path",
            base_path: "/music",
            expected: "http://music.lan:8688/music",
            ..plain
        },
        plain,
    ];
    for case in cases {
        let mut request = Request::builder()
            .uri("/jellyfin/System/Info/Public")
            .header("host", "music.lan:8688");
        if case.forwarded {
            request = request
                .header("x-forwarded-host", "music.example.com")
                .header("x-forwarded-proto", "https");
        }
        let mut request = request.body(Body::empty()).expect("request builds");
        let peer: std::net::SocketAddr = case.peer.parse().expect("peer address");
        request.extensions_mut().insert(ConnectInfo(peer));
        let listener = ListenerConfig {
            port: 8688,
            trusted_proxies: TrustedProxies::default(),
            base_path: case.base_path.to_owned(),
        };
        store
            .save_blocking(PublicAddress {
                origins: case.stated.iter().map(|&entry| entry.to_owned()).collect(),
            })
            .expect("stated addresses save");
        assert_eq!(
            base_address(&request, &listener, &settings).as_deref(),
            Some(case.expected),
            "{}",
            case.name
        );
    }
}

/// A picture signature is the whole lowercase hex MAC of the kind and id
/// under its own purpose. The same message signed under another purpose
/// does not open a picture, an album's signature does not open the artist
/// with the same id, and a cut or uppercased signature does not verify.
#[test]
fn picture_signatures_are_bound_to_their_purpose_and_kind() {
    let crypto = std::sync::Arc::new(Crypto::from_key_bytes(&[5u8; 32]).expect("test key"));
    let signer = PictureSigner::new(crypto.clone());
    let hex = |mac: [u8; 32]| mac.iter().map(|b| format!("{b:02x}")).collect::<String>();

    let album = signer.sign(PictureKind::Album, "rg-1");
    assert_eq!(album, hex(crypto.mac(PURPOSE, b"album\0rg-1")));
    assert!(signer.verifies(PictureKind::Album, "rg-1", &album));

    let elsewhere = hex(crypto.mac("catalog-correction-preview", b"album\0rg-1"));
    assert!(!signer.verifies(PictureKind::Album, "rg-1", &elsewhere));
    assert!(!signer.verifies(PictureKind::Artist, "rg-1", &album));
    assert!(!signer.verifies(PictureKind::Album, "rg-1", &album[..32]));
    assert!(!signer.verifies(PictureKind::Album, "rg-1", &album.to_uppercase()));
}
