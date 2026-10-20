//! Recorded third-party answers. Each row's fixture decodes through its
//! provider's client, and the same answer without one of its identity fields
//! fails decoding as a payload error. A provider adds a row per identity field
//! when its models land.

use std::sync::Arc;

use droppedneedle_providers::executor::ScriptedWire;
use droppedneedle_providers::geocoding::DEFAULT_CITY_COUNT;
use droppedneedle_providers::{
    PayloadKind, ProviderEndpoints, ProviderError, Providers, RequestPriority,
};

const PRIORITY: RequestPriority = RequestPriority::UserInitiated;

/// The provider whose client reads a row's answer.
#[derive(Clone, Copy)]
enum Provider {
    Lrclib,
    Geocoding,
}

/// One recorded answer.
struct Row {
    name: &'static str,
    provider: Provider,
    /// The recorded body, as the provider sent it.
    body: &'static str,
    /// A JSON Pointer to the field that identifies the answer. Without it,
    /// decoding fails.
    identity: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        name: "lrclib /api/get",
        provider: Provider::Lrclib,
        body: include_str!("../fixtures/lrclib/get_found.json"),
        identity: "/id",
    },
    Row {
        name: "open-meteo /v1/search, latitude",
        provider: Provider::Geocoding,
        body: include_str!("../fixtures/geocoding/search_liverpool.json"),
        identity: "/results/0/latitude",
    },
    Row {
        name: "open-meteo /v1/search, longitude",
        provider: Provider::Geocoding,
        body: include_str!("../fixtures/geocoding/search_liverpool.json"),
        identity: "/results/0/longitude",
    },
];

/// Serves `body` from the provider's address and reads it through the
/// provider's client: whether the answer is usable, or the typed failure.
async fn read(provider: Provider, body: &str) -> Result<bool, ProviderError> {
    match provider {
        Provider::Lrclib => {
            let wire = Arc::new(ScriptedWire::new());
            wire.answer(
                "http://fake/lrclib/api/get",
                200,
                &[],
                body.as_bytes().to_vec(),
            );
            let providers = Providers::unpaced_at(
                wire,
                ProviderEndpoints {
                    lrclib: "http://fake/lrclib".to_owned(),
                    ..ProviderEndpoints::closed()
                },
            );
            let lookup = providers
                .lrclib
                .get_exact_lyrics(
                    "Bohemian Rhapsody",
                    "Queen",
                    "A Night at the Opera",
                    355,
                    PRIORITY,
                )
                .await?;
            Ok(lookup.found)
        }
        Provider::Geocoding => {
            let wire = Arc::new(ScriptedWire::new());
            wire.answer(
                "http://fake/geocoding/v1/search",
                200,
                &[],
                body.as_bytes().to_vec(),
            );
            let providers = Providers::unpaced_at(
                wire,
                ProviderEndpoints {
                    geocoding: "http://fake/geocoding/v1/search".to_owned(),
                    ..ProviderEndpoints::closed()
                },
            );
            let cities = providers
                .geocoding
                .search_cities("Liverpool", DEFAULT_CITY_COUNT, PRIORITY)
                .await?;
            Ok(!cities.is_empty())
        }
    }
}

#[tokio::test]
async fn recorded_answers_decode_and_need_their_identity() {
    for row in ROWS {
        let usable = read(row.provider, row.body).await;
        assert!(matches!(usable, Ok(true)), "{}: {usable:?}", row.name);

        let mut answer: serde_json::Value =
            serde_json::from_str(row.body).expect("recorded body parses");
        let (parent, field) = row
            .identity
            .rsplit_once('/')
            .expect("identity is a JSON Pointer");
        answer
            .pointer_mut(parent)
            .and_then(serde_json::Value::as_object_mut)
            .expect("the identity's parent is an object")
            .remove(field)
            .expect("the recorded body carries its identity");
        let error = read(row.provider, &answer.to_string())
            .await
            .expect_err(row.name);
        assert!(
            matches!(
                error,
                ProviderError::Payload {
                    kind: PayloadKind::Decode,
                    ..
                }
            ),
            "{}: without `{}` the answer must fail decoding: {error:?}",
            row.name,
            row.identity
        );
    }
}
