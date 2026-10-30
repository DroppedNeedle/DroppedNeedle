//! Pwned Passwords, the breach corpus behind the password screen: one
//! k-anonymity range lookup, where only the first five hex characters of
//! the password's SHA-1 leave the server.
//!
//! Every lookup goes through the executor at the `hibp` rate row. The
//! answer is plain text, one `SUFFIX:COUNT` line per hash in the range.
//! The request asks for padding (`Add-Padding: true`), and the padded
//! lines, which always carry a count of 0, are dropped. Checked against
//! <https://haveibeenpwned.com/API/v3> on 2026-10-30.

use std::collections::HashSet;
use std::time::Duration;

use crate::RequestPriority;
use crate::client::{parse_base, unusable_base};
use crate::error::ProviderError;
use crate::executor::{Backoff, Executor, Request, Spec, Url, standard_classify};

/// The range API, up to the hash prefix.
pub const RANGE_BASE: &str = "https://api.pwnedpasswords.com/range";
/// The provider name errors carry, and its rate row.
pub const SOURCE: &str = "hibp";
/// Largest range answer read; a padded range is about 40 KB.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// The range API on the wire: the standard rules with a ten second
/// deadline. It documents no rate limit, so a 429 that names no wait
/// rests the row five seconds.
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(10),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(5)),
    retry_after_cap: Duration::from_secs(60),
    classify: standard_classify,
};

/// The Pwned Passwords client. Cheap to clone; clones share the row.
#[derive(Debug, Clone)]
pub struct HibpClient {
    executor: Executor,
    base: Option<Url>,
}

impl HibpClient {
    /// Calls through `executor`, on the `hibp` row, to the range API at
    /// `base`.
    #[must_use]
    pub fn new(executor: Executor, base: &str) -> Self {
        Self {
            executor,
            base: parse_base(base, SOURCE),
        }
    }

    /// The uppercase hash suffixes the corpus holds under `prefix`, the
    /// first five hex characters of a SHA-1.
    ///
    /// # Errors
    ///
    /// The executor's typed failure.
    pub async fn range(
        &self,
        prefix: &str,
        priority: RequestPriority,
    ) -> Result<HashSet<String>, ProviderError> {
        let base = self.base.as_ref().ok_or_else(|| unusable_base(SOURCE))?;
        let request = Request::get(base, &[prefix], &[])
            .map_err(|_| unusable_base(SOURCE))?
            .header("Add-Padding", "true");
        let reply = self.executor.send(request, priority).await?;
        Ok(suffixes(&String::from_utf8_lossy(reply.bytes())))
    }
}

/// Each `SUFFIX:COUNT` line's suffix, uppercased, skipping padding (a
/// count of 0) and lines that do not read.
fn suffixes(text: &str) -> HashSet<String> {
    text.lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(_, count)| count.trim() != "0")
        .map(|(suffix, _)| suffix.trim().to_uppercase())
        .filter(|suffix| !suffix.is_empty())
        .collect()
}
