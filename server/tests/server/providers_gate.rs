//! The provider rules hold in the source. This gate reads the shipping
//! source of every workspace crate as text, comment lines and unit test
//! modules aside, and fails on three shapes:
//!
//! - a rate written outside the providers crate's rate table: a token
//!   bucket, a rate gate, a semaphore or a `_PER_SEC`, `_SPACING` or
//!   `_RATE_LIMIT` constant;
//! - a provider file that reaches reqwest or the kernel's HTTP clients
//!   instead of sending through the executor;
//! - a request priority nobody chose: a `Default` for `RequestPriority`,
//!   a constructor or builder that sets one, or an `unwrap_or` that falls
//!   back to one.
//!
//! What remains of the first two is listed by file with its reason, and
//! every listed file must still match its rule.

use regex::Regex;

use crate::common::sources::workspace_sources;

struct Rule {
    what: &'static str,
    /// Only files whose path starts with this are read.
    under: &'static str,
    patterns: &'static [&'static str],
    allowed: &'static [(&'static str, &'static str)],
}

const LOGIN_LIMITER: &str =
    "the sign-in limiter, which caps attempts against this server, not an outside service";
const LOCAL: &str = "local concurrency, not an outside rate";

const RULES: &[Rule] = &[
    Rule {
        what: "a rate outside the rate table",
        under: "",
        patterns: &[
            r"RatePolicy::new\(",
            r"RateLimiter::new\(",
            r"\bRateGate\b",
            r"Semaphore::new\(",
            r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?const\s+\w*(?:_PER_SEC|_SPACING|_RATE_LIMIT)\b",
        ],
        allowed: &[
            (
                "providers/src/limiter.rs",
                "the limiter itself: a bucket built from a table row, and the one-a-second \
                 fallback for an executor whose row is missing",
            ),
            (
                "kernel/src/runtime_config/sections/musicbrainz.rs",
                "the MusicBrainz rates Settings offers and checks; the kernel sits below the \
                 providers crate and cannot read its table",
            ),
            ("auth/src/session/middleware.rs", LOGIN_LIMITER),
            ("auth/src/wiring.rs", LOGIN_LIMITER),
            ("auth/src/routes/native.rs", LOGIN_LIMITER),
            ("auth/src/routes/federated.rs", LOGIN_LIMITER),
            (
                "plugins/src/wiring.rs",
                "the limiter on inbound plugin routes, not an outside service",
            ),
            (
                "plugins/src/capabilities/events.rs",
                "how many events a plugin may publish here, not an outside service",
            ),
            ("plugins/src/process.rs", LOCAL),
            (
                "src/bootstrap/jobs.rs",
                "the scripted job kind's gate, which holds a run until a test releases it",
            ),
            ("catalog/src/reads/platform/artwork/mod.rs", LOCAL),
            ("catalog/src/reads/discover/listening/mod.rs", LOCAL),
            (
                "acquire/src/slskd/repository.rs",
                "slskd runs one search and one enqueue at a time; a LAN service is never paced",
            ),
            ("library/src/scan/pool.rs", LOCAL),
            (
                "media/src/remotes/connections.rs",
                "how many reads people trigger run at once against one media server, a memory \
                 bound on capped answers, not a rate",
            ),
            ("kernel/src/db/cache/mod.rs", LOCAL),
        ],
    },
    Rule {
        what: "an HTTP client in a provider file",
        under: "providers/src/",
        patterns: &[r"\breqwest\b", r"\bHttpClientFactory\b", r"\bhttp_client::"],
        allowed: &[(
            "providers/src/executor.rs",
            "the production wire, the one place a provider request meets reqwest and the \
             kernel's HTTP clients",
        )],
    },
    Rule {
        what: "a request priority nobody chose",
        under: "",
        patterns: &[
            r"impl\s+Default\s+for\s+(?:\w+::)*RequestPriority\b",
            r"(?s)enum\s+RequestPriority\s*\{[^}]*#\[default\]",
            r"\bfn\s+with_priority\b",
            r"(?s)\bfn\s+(?:new|default|builder)\b[^{;]*\{[^}]*?\bpriority:\s*(?:\w+::)*RequestPriority::\w+",
            r"unwrap_or(?:_else)?\(\s*(?:\|\|\s*)?(?:\w+::)*RequestPriority::",
        ],
        allowed: &[],
    },
];

#[test]
fn providers_keep_their_rates_wire_and_priorities() {
    let sources = workspace_sources();
    let mut wrong = Vec::new();
    for rule in RULES {
        let patterns: Vec<Regex> = rule
            .patterns
            .iter()
            .map(|pattern| Regex::new(pattern).unwrap())
            .collect();
        let mut used = vec![false; rule.allowed.len()];
        for source in sources
            .iter()
            .filter(|source| source.path.starts_with(rule.under))
        {
            let code = source.shipping_code();
            for pattern in &patterns {
                for found in pattern.find_iter(&code) {
                    match rule
                        .allowed
                        .iter()
                        .position(|(path, _)| source.path == *path)
                    {
                        Some(at) => used[at] = true,
                        None => {
                            let line = code[..found.start()].matches('\n').count() + 1;
                            let text = found.as_str().split_whitespace().collect::<Vec<_>>();
                            wrong.push(format!(
                                "{}:{line}: {} ({})",
                                source.path,
                                text.join(" "),
                                rule.what
                            ));
                        }
                    }
                }
            }
        }
        for ((path, why), used) in rule.allowed.iter().zip(used) {
            if !used {
                wrong.push(format!(
                    "{path}: listed for {}, but nothing there is one; remove the entry ({why})",
                    rule.what
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "write the rate in the providers' rate table, send through the executor, and \
         have every caller name its priority:\n{}",
        wrong.join("\n")
    );
}
