//! Shared compat security posture: which Jellyfin paths are anonymous,
//! query redaction, and the rate-limit and auth-backoff budgets. The
//! credential schemes are pinned in `auth_compat_goldens`.

use droppedneedle::compat::shared::{auth, ratelimit, redact};

#[test]
fn anonymous_paths() {
    // Only these Jellyfin routes skip auth: public info, login, images, audio.
    let anonymous = [
        ("GET", "/jellyfin/System/Info/Public"),
        ("GET", "/jellyfin/QuickConnect/Enabled"),
        ("POST", "/jellyfin/Sessions/Logout"),
        ("POST", "/jellyfin/Users/AuthenticateByName"),
        ("POST", "/jellyfin/users/authenticatebyname"),
        ("GET", "/jellyfin/Items/abc/Images/Primary"),
        ("GET", "/jellyfin/Items/abc/Images/Primary/0"),
    ];
    for (method, path) in anonymous {
        assert!(auth::jellyfin_is_anonymous(method, path), "{method} {path}");
    }
    let authed = [
        ("GET", "/jellyfin/System/Info"),
        ("GET", "/jellyfin/Users/me"),
        ("GET", "/jellyfin/Items"),
        ("GET", "/jellyfin/UserItems/Latest"),
        ("GET", "/jellyfin/Items/abc"),
        ("POST", "/jellyfin/Items/abc/PlaybackInfo"),
        ("GET", "/jellyfin/Audio/abc"),
        ("GET", "/jellyfin/Audio/abc/universal"),
        ("GET", "/jellyfin/Audio/abc/stream.mp3"),
        ("HEAD", "/jellyfin/Audio/abc/stream"),
        ("DELETE", "/jellyfin/Items/abc/Images/Primary"),
    ];
    for (method, path) in authed {
        assert!(
            !auth::jellyfin_is_anonymous(method, path),
            "{method} {path}"
        );
    }
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
