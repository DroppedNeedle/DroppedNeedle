//! Stage-7 Usenet acquisition: SABnzbd + Newznab + Prowlarr.
//!
//! The download half of Usenet lives here: [`sabnzbd`] enqueues NZBs and
//! tracks jobs, [`newznab`] and [`prowlarr`] search indexers for releases.
//! [`policy`] carries the slice's quality tiers, search recipe, timeouts,
//! and retention gates, ported from v2's `DownloadPolicySettings` and
//! `quality_tiers`; [`mocks`] serves the loopback SAB/Newznab/Prowlarr
//! servers the contract briefs run against. Nothing here touches the live
//! network: every brief runs against `127.0.0.1` mocks.
//!
//! HTTP seam: each client takes an injected `reqwest::Client` (built once
//! from the stage-1 `HttpClientFactory`), mirroring v2's injected httpx
//! client (AUD-12). The stage-5 catalog `HttpPort` is GET-only and does
//! not fit SABnzbd's multipart POST, so the slice keeps this minimal seam.
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
pub mod xml;
