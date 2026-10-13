//! Usenet acquisition: SABnzbd + Newznab + Prowlarr.
//!
//! The download half of Usenet lives here: [`sabnzbd`] enqueues NZBs and
//! tracks jobs, [`newznab`] and [`prowlarr`] search indexers for releases.
//! [`policy`] carries the Usenet quality tiers, search recipe, timeouts,
//! and retention gates, ported from v2's `DownloadPolicySettings` and
//! `quality_tiers`; [`mocks`] serves the loopback SAB/Newznab/Prowlarr
//! servers the contract tests run against. No test touches the live
//! network: every one runs against `127.0.0.1` mocks.
//!
//! HTTP seam: each client takes an injected `reqwest::Client` (built once
//! from the shared `HttpClientFactory`), mirroring v2's injected httpx
//! client. The provider catalog `HttpPort` is GET-only and does not fit
//! SABnzbd's multipart POST, so Usenet keeps this minimal seam.
//!
//! Live-version-cited quirks ported from v2 (each cited at its call site):
//! SABnzbd 5.0.4 suffix params + `cat`-not-`category` + multipart addfile
//! with the addurl fallback, the JSON/`"False"`/plain-text error forms,
//! Newznab's XML-only feed with the 202 `t=music` → `t=search` fallback,
//! and Prowlarr 2.3.5.5327's header auth with the never-logged key.

#[cfg(any(test, feature = "test-support"))]
pub mod mocks;
pub mod newznab;
pub mod policy;
pub mod prowlarr;
pub mod sabnzbd;
pub mod sabnzbd_models;
pub mod xml;

use crate::http_client::{BodyError, read_capped};

/// The most an indexer response may weigh before it is refused. A search
/// page is at most a few megabytes even from an indexer that allows 1000
/// results a page (about 3 KB an item with extended attributes), and a
/// caps document is tens of kilobytes, so 10 MB refuses nothing real and
/// still stops an indexer from filling memory.
pub const MAX_INDEXER_BODY_BYTES: usize = 10 * 1024 * 1024;

/// Read an indexer body under [`MAX_INDEXER_BODY_BYTES`].
pub(crate) async fn read_indexer_body(response: reqwest::Response) -> Result<Vec<u8>, BodyError> {
    read_capped(response, MAX_INDEXER_BODY_BYTES).await
}
