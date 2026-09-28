//! Stage 3 compat-auth goldens: every Subsonic code, the binary split
//! with the getAvatar 403-as-text exception, Jellyfin 401s with the login
//! echo, and account-password rejection on both protocols.

#[allow(dead_code)]
use droppedneedle::auth::compat_auth::fakes::FakeCompatPasswords;
use droppedneedle::auth::compat_auth::jellyfin::{
    JellyfinRequest, JellyfinUser, MAX_AUTH_VALUE_LENGTH, SessionFacts, UNAUTHORIZED,
    authenticate_by_name, effective_name, extract_client, extract_device, extract_token,
    login_echo_json, resolve_token, server_id,
};
use droppedneedle::auth::compat_auth::subsonic::{
    API_VERSION, AUTH_CODES, AVATAR_FORBIDDEN_MESSAGE, AppSecret, BINARY_ENDPOINTS,
    CONFLICTING_AUTH, GENERIC, INVALID_APIKEY, MAX_CLIENT_NAME_LENGTH, MAX_ENCODED_PASSWORD_LENGTH,
    NAMESPACE, NOT_AUTHORIZED, NOT_FOUND, PARAM_MISSING, PUBLIC_ENDPOINT, SubsonicDenied,
    SubsonicFormat, SubsonicParams, SubsonicPasswordStore, SubsonicStoreError, WRONG_CREDENTIALS,
    authenticate, avatar_is_self, callback_is_safe, decode_subsonic_password, default_message,
    dispatch_uses_envelope, hex_encode, is_binary_endpoint, is_public_endpoint, md5_hex,
    normalize_endpoint, parse_format, render_binary_error, render_error, sha256_hex,
};

fn passwords() -> FakeCompatPasswords {
    let store = FakeCompatPasswords::new();
    store.add_user(
        "user-1",
        "alice",
        "Alice",
        "user",
        "alice-account-password",
        &["alice-secret"],
    );
    store.add_user(
        "user-2",
        "bob",
        "Bob",
        "admin",
        "bob-account-password",
        &["bob-secret"],
    );
    store
}

#[test]
fn protocol_constants_match_v2() {
    assert_eq!(API_VERSION, "1.16.1");
    assert_eq!(NAMESPACE, "http://subsonic.org/restapi");
    assert_eq!(PUBLIC_ENDPOINT, "getopensubsonicextensions");
    assert_eq!(BINARY_ENDPOINTS.len(), 5);
    assert_eq!(AUTH_CODES, &[10, 40, 41, 42, 43, 44, 50]);
    assert_eq!(MAX_ENCODED_PASSWORD_LENGTH, 2052);
    assert_eq!(MAX_AUTH_VALUE_LENGTH, 1024);
    assert_eq!(UNAUTHORIZED, 401);
    assert_eq!(server_id(), "2f54b621b8fde6d933fab26bd6378f85");
}

#[test]
fn default_messages_match_v2_verbatim() {
    assert_eq!(default_message(GENERIC), "An error occurred.");
    assert_eq!(
        default_message(PARAM_MISSING),
        "Required parameter is missing."
    );
    assert_eq!(
        default_message(WRONG_CREDENTIALS),
        "Wrong username or password."
    );
    assert_eq!(
        default_message(CONFLICTING_AUTH),
        "Multiple conflicting authentication mechanisms provided."
    );
    assert_eq!(default_message(INVALID_APIKEY), "Invalid API key.");
    assert_eq!(
        default_message(NOT_AUTHORIZED),
        "User is not authorized for the given operation."
    );
    assert_eq!(
        default_message(NOT_FOUND),
        "The requested data was not found."
    );
    assert_eq!(default_message(99), "An error occurred.");
}

#[test]
fn error_envelope_json_golden() {
    let rendered = render_error(
        WRONG_CREDENTIALS,
        default_message(WRONG_CREDENTIALS),
        SubsonicFormat::Json,
        None,
        "DroppedNeedle",
        "3.0.0",
    );
    assert_eq!(rendered.status, 200);
    assert_eq!(rendered.content_type, "application/json");
    assert_eq!(
        rendered.body_text(),
        "{\"subsonic-response\":{\"status\":\"failed\",\"version\":\"1.16.1\",\"type\":\"DroppedNeedle\",\"serverVersion\":\"3.0.0\",\"openSubsonic\":true,\"error\":{\"code\":40,\"message\":\"Wrong username or password.\"}}}"
    );
}

#[test]
fn error_envelope_xml_golden() {
    let rendered = render_error(
        PARAM_MISSING,
        default_message(PARAM_MISSING),
        SubsonicFormat::Xml,
        None,
        "DroppedNeedle",
        "3.0.0",
    );
    assert_eq!(rendered.status, 200);
    assert_eq!(rendered.content_type, "application/xml");
    assert_eq!(
        rendered.body_text(),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><subsonic-response xmlns=\"http://subsonic.org/restapi\" status=\"failed\" version=\"1.16.1\" type=\"DroppedNeedle\" serverVersion=\"3.0.0\" openSubsonic=\"true\"><error code=\"10\" message=\"Required parameter is missing.\"/></subsonic-response>"
    );
}

#[test]
fn error_envelope_escapes_and_jsonp_falls_back() {
    let rendered = render_error(
        GENERIC,
        "a&b<\"c\">",
        SubsonicFormat::Xml,
        None,
        "DroppedNeedle",
        "3.0.0",
    );
    assert!(
        rendered
            .body_text()
            .contains("message=\"a&amp;b&lt;&quot;c&quot;&gt;\"")
    );

    let jsonp = render_error(
        GENERIC,
        "x",
        SubsonicFormat::Jsonp,
        Some("cb.ok$1"),
        "D",
        "3.0.0",
    );
    assert_eq!(jsonp.content_type, "application/javascript");
    assert!(
        jsonp
            .body_text()
            .starts_with("cb.ok$1({\"subsonic-response\":")
    );
    assert!(jsonp.body_text().ends_with("});"));

    for bad in [None, Some("alert(1)"), Some(""), Some("has space")] {
        let fallback = render_error(GENERIC, "x", SubsonicFormat::Jsonp, bad, "D", "3.0.0");
        assert_eq!(fallback.content_type, "application/json");
    }
    assert!(callback_is_safe("a"));
    assert!(!callback_is_safe("9a"));
    assert!(!callback_is_safe("a-b"));
    assert_eq!(parse_format(None), Ok(SubsonicFormat::Xml));
    assert_eq!(
        parse_format(Some("yaml")),
        Err(SubsonicDenied::new(PARAM_MISSING))
    );
}

#[test]
fn endpoint_normalization_and_sets_match_v2() {
    assert_eq!(normalize_endpoint("getArtists.view"), "getartists");
    assert_eq!(normalize_endpoint("STREAM.VIEW"), "stream");
    assert_eq!(normalize_endpoint("ping.view.view"), "ping.view");
    assert!(is_binary_endpoint("stream"));
    assert!(is_binary_endpoint("getavatar"));
    assert!(!is_binary_endpoint("ping"));
    assert!(is_public_endpoint("getopensubsonicextensions"));
    assert!(!is_public_endpoint("ping"));
}

#[test]
fn dispatch_split_keeps_50_enveloped_and_sends_the_rest_to_text() {
    assert!(dispatch_uses_envelope(WRONG_CREDENTIALS, "stream"));
    assert!(dispatch_uses_envelope(NOT_AUTHORIZED, "stream"));
    assert!(!dispatch_uses_envelope(NOT_FOUND, "stream"));
    assert!(!dispatch_uses_envelope(GENERIC, "download"));
    assert!(dispatch_uses_envelope(NOT_FOUND, "getartists"));
    assert!(dispatch_uses_envelope(NOT_AUTHORIZED, "getartists"));

    let text = render_binary_error(NOT_FOUND, "missing");
    assert_eq!((text.status, text.content_type), (404, "text/plain"));
    assert_eq!(text.body_text(), "missing");
    assert_eq!(render_binary_error(NOT_AUTHORIZED, "no").status, 403);
    assert_eq!(render_binary_error(GENERIC, "boom").status, 404);

    assert_eq!(
        AVATAR_FORBIDDEN_MESSAGE,
        "Avatar access is limited to the authenticated user"
    );
    assert!(avatar_is_self(
        &["alice", "Alice", "Alice Liddell"],
        Some("ALICE")
    ));
    assert!(!avatar_is_self(
        &["alice", "Alice", "Alice Liddell"],
        Some("bob")
    ));
    assert!(!avatar_is_self(&["alice"], None));
}

#[tokio::test]
async fn token_scheme_verifies_and_rejects() {
    let store = passwords();
    let salt = "pepper";
    let token = md5_hex(&format!("alice-secret{salt}"));
    let params = SubsonicParams::new(vec![
        ("u", "alice"),
        ("t", &token),
        ("s", salt),
        ("c", "symfonium"),
    ]);
    assert_eq!(
        authenticate(&store, &params).await.unwrap().user_id,
        "user-1"
    );
    let touches = store.touches();
    assert_eq!(touches.len(), 1);
    assert_eq!(touches[0].1.as_deref(), Some("symfonium"));

    let params = SubsonicParams::new(vec![("u", "alice"), ("t", &md5_hex("nope")), ("s", salt)]);
    assert_eq!(
        authenticate(&store, &params).await,
        Err(SubsonicDenied::new(WRONG_CREDENTIALS))
    );

    let impostor = md5_hex(&format!("alice-account-password{salt}"));
    let params = SubsonicParams::new(vec![("u", "alice"), ("t", &impostor), ("s", salt)]);
    assert_eq!(
        authenticate(&store, &params).await,
        Err(SubsonicDenied::new(WRONG_CREDENTIALS))
    );
}

#[tokio::test]
async fn password_scheme_handles_plain_hex_and_rejection() {
    let store = passwords();
    let params = SubsonicParams::new(vec![("u", "ALICE"), ("p", "alice-secret")]);
    assert_eq!(
        authenticate(&store, &params).await.unwrap().user_id,
        "user-1"
    );

    let hexed = format!("enc:{}", hex_encode(b"alice-secret"));
    let params = SubsonicParams::new(vec![("u", "alice"), ("p", &hexed)]);
    assert_eq!(
        authenticate(&store, &params).await.unwrap().user_id,
        "user-1"
    );

    assert_eq!(
        decode_subsonic_password("enc:616c6963652d736563726574"),
        "alice-secret"
    );
    let params = SubsonicParams::new(vec![("u", "alice"), ("p", "enc:zzz")]);
    assert_eq!(
        authenticate(&store, &params).await,
        Err(SubsonicDenied::new(WRONG_CREDENTIALS))
    );

    let params = SubsonicParams::new(vec![("u", "alice"), ("p", "")]);
    assert_eq!(
        authenticate(&store, &params).await,
        Err(SubsonicDenied::new(WRONG_CREDENTIALS))
    );

    let unknown = SubsonicParams::new(vec![("u", "nobody"), ("p", "whatever")]);
    let account = SubsonicParams::new(vec![("u", "alice"), ("p", "alice-account-password")]);
    assert_eq!(
        authenticate(&store, &unknown).await,
        authenticate(&store, &account).await
    );
    assert_eq!(
        authenticate(&store, &account).await,
        Err(SubsonicDenied::new(WRONG_CREDENTIALS))
    );
}

#[tokio::test]
async fn subsonic_username_is_stripped_before_lookup() {
    let store = passwords();
    let params = SubsonicParams::new(vec![("u", " alice "), ("p", "alice-secret")]);
    assert_eq!(
        authenticate(&store, &params).await.unwrap().user_id,
        "user-1"
    );
}

#[test]
fn jsonp_callback_allows_unicode_word_chars_and_counts_chars() {
    assert!(callback_is_safe("cbé.ok$1"));
    assert!(callback_is_safe(&format!("a{}", "é".repeat(127))));
    assert!(!callback_is_safe(&format!("a{}", "é".repeat(128))));
    assert!(!callback_is_safe("ébc"));
}

#[tokio::test]
async fn apikey_scheme_is_lone_and_rejects_account_passwords() {
    let store = passwords();
    let params = SubsonicParams::new(vec![("apiKey", "bob-secret")]);
    assert_eq!(
        authenticate(&store, &params).await.unwrap().user_id,
        "user-2"
    );
    assert_eq!(store.touches()[0].1, None);

    let params = SubsonicParams::new(vec![("apiKey", "nope")]);
    assert_eq!(
        authenticate(&store, &params).await,
        Err(SubsonicDenied::new(INVALID_APIKEY))
    );

    let params = SubsonicParams::new(vec![("apiKey", "bob-account-password")]);
    assert_eq!(
        authenticate(&store, &params).await,
        Err(SubsonicDenied::new(INVALID_APIKEY))
    );

    let params = SubsonicParams::new(vec![("apiKey", "bob-secret"), ("u", "bob")]);
    assert_eq!(
        authenticate(&store, &params).await,
        Err(SubsonicDenied::new(CONFLICTING_AUTH))
    );
}

#[tokio::test]
async fn conflicts_duplicates_and_missing_params_are_code_10() {
    let store = passwords();
    for entries in [
        vec![("t", "a"), ("s", "b")],
        vec![("u", "alice"), ("u", "bob")],
        vec![("u", "alice"), ("t", "a")],
        vec![("u", "alice"), ("p", "x"), ("t", "a"), ("s", "b")],
        vec![("apiKey", "k"), ("t", "a"), ("s", "b")],
        vec![("u", "alice")],
    ] {
        let params = SubsonicParams::new(entries);
        assert_eq!(
            authenticate(&store, &params).await,
            Err(SubsonicDenied::new(PARAM_MISSING)),
            "entries must be code 10"
        );
    }
    let long_client = "c".repeat(MAX_CLIENT_NAME_LENGTH + 1);
    let params = SubsonicParams::new(vec![
        ("u", "alice"),
        ("p", "alice-secret"),
        ("c", &long_client),
    ]);
    assert_eq!(
        authenticate(&store, &params).await,
        Err(SubsonicDenied::new(PARAM_MISSING))
    );
}

#[tokio::test]
async fn store_failure_is_generic_never_an_auth_code() {
    #[derive(Debug, Clone, Default)]
    struct Broken;
    impl SubsonicPasswordStore for Broken {
        async fn user_id_for_username(
            &self,
            _: &str,
        ) -> Result<Option<String>, SubsonicStoreError> {
            Err(SubsonicStoreError)
        }
        async fn active_secrets(&self, _: &str) -> Result<Vec<AppSecret>, SubsonicStoreError> {
            Err(SubsonicStoreError)
        }
        async fn owner_of_secret(&self, _: &str) -> Result<Option<String>, SubsonicStoreError> {
            Err(SubsonicStoreError)
        }
        async fn note_use(&self, _: &str, _: Option<&str>) {}
    }
    let params = SubsonicParams::new(vec![("u", "alice"), ("p", "alice-secret")]);
    assert_eq!(
        authenticate(&Broken, &params).await,
        Err(SubsonicDenied::new(GENERIC))
    );
}

#[test]
fn denial_renders_through_the_envelope() {
    let denied = SubsonicDenied::new(NOT_FOUND);
    assert_eq!(denied.message(), "The requested data was not found.");
    let rendered = denied.render(SubsonicFormat::Json, None, "DroppedNeedle", "3.0.0");
    assert!(rendered.body_text().contains("\"code\":70"));
    assert_eq!(
        sha256_hex("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        md5_hex("alice-secretpepper"),
        "22b231fb88294b6a25be3d978438fdc1"
    );
}

#[test]
fn jellyfin_token_extraction_order_is_pinned() {
    let full = JellyfinRequest {
        authorization: Some("MediaBrowser Token=\"header\", Client=\"Finamp\""),
        emby_authorization: Some("MediaBrowser Token=\"legacy\""),
        emby_token: Some("direct"),
        mediabrowser_token: Some("browser"),
        query_apikey: Some("query"),
        query_api_key: Some("query-lower"),
    };
    assert_eq!(extract_token(&full).as_deref(), Some("header"));

    let legacy = JellyfinRequest {
        authorization: None,
        emby_authorization: Some("MediaBrowser Token=\"legacy\""),
        emby_token: Some("direct"),
        ..JellyfinRequest::default()
    };
    assert_eq!(extract_token(&legacy).as_deref(), Some("legacy"));

    let direct = JellyfinRequest {
        emby_token: Some("direct"),
        mediabrowser_token: Some("browser"),
        query_apikey: Some("query"),
        ..JellyfinRequest::default()
    };
    assert_eq!(extract_token(&direct).as_deref(), Some("direct"));

    let query = JellyfinRequest {
        query_apikey: Some("query"),
        query_api_key: Some("query-lower"),
        ..JellyfinRequest::default()
    };
    assert_eq!(extract_token(&query).as_deref(), Some("query"));
    let lower = JellyfinRequest {
        query_api_key: Some("query-lower"),
        ..JellyfinRequest::default()
    };
    assert_eq!(extract_token(&lower).as_deref(), Some("query-lower"));

    assert_eq!(extract_token(&JellyfinRequest::default()), None);
    let empty = JellyfinRequest {
        authorization: Some("MediaBrowser Token=\"\""),
        ..JellyfinRequest::default()
    };
    assert_eq!(extract_token(&empty), None);

    assert_eq!(extract_client(&full).as_deref(), Some("Finamp"));
    let device = JellyfinRequest {
        authorization: Some("MediaBrowser Device=\"Pixel\", DeviceId=\"dev-1\""),
        ..JellyfinRequest::default()
    };
    assert_eq!(
        extract_device(&device),
        (Some("Pixel".to_owned()), Some("dev-1".to_owned()))
    );
    assert_eq!(extract_device(&JellyfinRequest::default()), (None, None));
}

#[test]
fn empty_authorization_falls_through_to_legacy_header() {
    let request = JellyfinRequest {
        authorization: Some(""),
        emby_authorization: Some("MediaBrowser Token=\"legacy\", Client=\"Jellify\""),
        ..JellyfinRequest::default()
    };
    assert_eq!(extract_token(&request).as_deref(), Some("legacy"));
    assert_eq!(extract_client(&request).as_deref(), Some("Jellify"));
}

#[test]
fn jellyfin_effective_name_treats_empty_as_absent() {
    let blank_display = JellyfinUser {
        id: "user-1".to_owned(),
        username: Some("alice".to_owned()),
        username_display: Some(String::new()),
        display_name: "Alice Liddell".to_owned(),
        role: "user".to_owned(),
    };
    assert_eq!(effective_name(&blank_display), "alice");
    let blank_both = JellyfinUser {
        username: Some(String::new()),
        username_display: None,
        ..blank_display
    };
    assert_eq!(effective_name(&blank_both), "Alice Liddell");
}

#[tokio::test]
async fn jellyfin_login_accepts_app_passwords_and_rejects_everything_else_identically() {
    let store = passwords();
    let user = authenticate_by_name(&store, " Alice ", "alice-secret", Some("finamp"))
        .await
        .unwrap();
    assert_eq!(user.id, "user-1");

    let bad_password = authenticate_by_name(&store, "alice", "wrong", None).await;
    let unknown_user = authenticate_by_name(&store, "nobody", "whatever", None).await;
    let account_password =
        authenticate_by_name(&store, "alice", "alice-account-password", None).await;
    let empty_password = authenticate_by_name(&store, "alice", "", None).await;
    for denied in [
        &bad_password,
        &unknown_user,
        &account_password,
        &empty_password,
    ] {
        let denied = denied.as_ref().unwrap_err();
        assert_eq!(denied.status(), 401);
        assert_eq!(denied.body(), b"".as_slice());
    }
    assert_eq!(bad_password, unknown_user);
    assert_eq!(account_password, unknown_user);
}

#[tokio::test]
async fn jellyfin_token_resolution_never_distinguishes_misses() {
    let store = passwords();
    let user = resolve_token(&store, Some("bob-secret")).await.unwrap();
    assert_eq!(user.id, "user-2");

    let missing = resolve_token(&store, None).await;
    let empty = resolve_token(&store, Some("")).await;
    let unknown = resolve_token(&store, Some("nope")).await;
    let account = resolve_token(&store, Some("bob-account-password")).await;
    let overlong = resolve_token(&store, Some(&"x".repeat(2048))).await;
    for denied in [&missing, &empty, &unknown, &account, &overlong] {
        let denied = denied.as_ref().unwrap_err();
        assert_eq!(denied.status(), 401);
        assert_eq!(denied.body(), b"".as_slice());
    }
    assert_eq!(missing, unknown);
    assert_eq!(account, unknown);
}

fn jellyfin_user() -> JellyfinUser {
    JellyfinUser {
        id: "user-7".to_owned(),
        username: Some("alice".to_owned()),
        username_display: Some("Alice".to_owned()),
        display_name: "Alice Liddell".to_owned(),
        role: "admin".to_owned(),
    }
}

fn session_facts() -> SessionFacts {
    SessionFacts {
        id: "0123456789abcdef0123456789abcdef".to_owned(),
        client: Some("Finamp".to_owned()),
        device_name: Some("Pixel".to_owned()),
        device_id: Some("dev-1".to_owned()),
        last_activity: "2026-09-28T12:00:00.123456+00:00".to_owned(),
    }
}

#[test]
fn jellyfin_login_echo_golden() {
    assert_eq!(effective_name(&jellyfin_user()), "Alice");
    let echo = login_echo_json(&jellyfin_user(), "pw-1", &session_facts());
    assert_eq!(
        echo,
        "{\"User\":{\"Id\":\"user-7\",\"Name\":\"Alice\",\"ServerId\":\"2f54b621b8fde6d933fab26bd6378f85\",\"HasPassword\":true,\"HasConfiguredPassword\":true,\"HasConfiguredEasyPassword\":false,\"Configuration\":{\"PlayDefaultAudioTrack\":true,\"DisplayMissingEpisodes\":false,\"GroupedFolders\":[],\"SubtitleMode\":\"Default\",\"DisplayCollectionsView\":false,\"EnableLocalPassword\":false,\"OrderedViews\":[],\"LatestItemsExcludes\":[],\"MyMediaExcludes\":[],\"HidePlayedInLatest\":true,\"RememberAudioSelections\":true,\"RememberSubtitleSelections\":true,\"EnableNextEpisodeAutoPlay\":true},\"Policy\":{\"IsAdministrator\":true,\"IsHidden\":false,\"IsDisabled\":false,\"EnableAllFolders\":true,\"EnabledFolders\":[],\"EnableAllChannels\":true,\"EnabledChannels\":[],\"EnableAllDevices\":true,\"EnabledDevices\":[],\"EnableMediaPlayback\":true,\"EnableAudioPlaybackTranscoding\":true,\"EnableVideoPlaybackTranscoding\":true,\"EnablePlaybackRemuxing\":true,\"EnableContentDownloading\":true,\"EnableRemoteAccess\":true,\"EnableSyncTranscoding\":true,\"EnableUserPreferenceAccess\":true,\"EnableLiveTvAccess\":false,\"EnableLiveTvManagement\":false,\"EnableContentDeletion\":false,\"EnableMediaConversion\":false,\"EnablePublicSharing\":false,\"EnableRemoteControlOfOtherUsers\":false,\"EnableSharedDeviceControl\":false,\"InvalidLoginAttemptCount\":0,\"RemoteClientBitrateLimit\":0,\"SyncPlayAccess\":\"CreateAndJoinGroups\",\"BlockedTags\":[],\"AllowedTags\":[],\"AccessSchedules\":[],\"BlockUnratedItems\":[]}},\"AccessToken\":\"pw-1\",\"SessionInfo\":{\"Id\":\"0123456789abcdef0123456789abcdef\",\"UserId\":\"user-7\",\"UserName\":\"Alice\",\"LastActivityDate\":\"2026-09-28T12:00:00.123456+00:00\",\"Client\":\"Finamp\",\"DeviceName\":\"Pixel\",\"DeviceId\":\"dev-1\",\"IsActive\":true,\"SupportsRemoteControl\":false,\"SupportsMediaControl\":false,\"HasCustomDeviceName\":false,\"ServerId\":\"2f54b621b8fde6d933fab26bd6378f85\"},\"ServerId\":\"2f54b621b8fde6d933fab26bd6378f85\"}"
    );
    let parsed: serde_json::Value = serde_json::from_str(&echo).unwrap();
    assert_eq!(parsed["AccessToken"], "pw-1");
    assert_eq!(parsed["User"]["Policy"]["EnableAllFolders"], true);
}

#[test]
fn jellyfin_login_echo_strips_missing_session_fields_and_echoes_verbatim() {
    let mut user = jellyfin_user();
    user.role = "user".to_owned();
    let facts = SessionFacts {
        id: "s".to_owned(),
        client: None,
        device_name: None,
        device_id: None,
        last_activity: "t".to_owned(),
    };
    let echo = login_echo_json(&user, "a\"b\\c", &facts);
    assert!(echo.contains("\"IsAdministrator\":false"));
    assert!(echo.contains("\"AccessToken\":\"a\\\"b\\\\c\""));
    assert!(!echo.contains("\"Client\""));
    assert!(!echo.contains("\"DeviceId\""));
    assert!(echo.contains("\"DeviceName\":\"\""));
    let parsed: serde_json::Value = serde_json::from_str(&echo).unwrap();
    assert_eq!(parsed["AccessToken"], "a\"b\\c");
}
