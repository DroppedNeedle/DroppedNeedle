//! The provider rules hold in the source. This gate reads the shipping
//! source of every workspace crate as text, comment lines and unit test
//! modules aside, and fails on three kinds of shape:
//!
//! - a rate written outside the providers crate's rate table: a token
//!   bucket, a rate policy, a rate gate, a semaphore, a renamed or aliased
//!   one of those, or a `const` or `static` named as a rate;
//! - an outbound HTTP client outside the providers' executor and the
//!   clients of services the user runs: holding or building a reqwest
//!   client or request, reaching the kernel's HTTP client factory, or
//!   sending a request (reading a body with `read_capped` is fine
//!   anywhere);
//! - a request priority nobody chose: a `Default` for `RequestPriority`,
//!   an alias of it, a constructor that sets one, or an `unwrap_or` or
//!   `map_or` that falls back to one.
//!
//! What remains of the first two is listed by file or folder, each entry
//! naming the shapes it may hold and why. A shape an entry does not name
//! still fails there, and every shape an entry names must still be found.

use regex::Regex;

use crate::common::sources::workspace_sources;

/// One shape a rule refuses.
struct Shape {
    name: &'static str,
    pattern: &'static str,
}

/// Where a rule's shapes may stay: a file, or a folder when the path ends
/// in `/`.
struct Allow {
    path: &'static str,
    shapes: &'static [&'static str],
    why: &'static str,
}

struct Rule {
    what: &'static str,
    shapes: &'static [Shape],
    allowed: &'static [Allow],
}

const LOGIN_LIMITER: &str =
    "the sign-in limiter, which caps attempts against this server, not an outside service";
const LOCAL: &str = "local concurrency, not an outside rate";
const UNIT: &str = "a unit conversion, not a rate";

const RATE: Rule = Rule {
    what: "a rate outside the rate table",
    shapes: &[
        Shape {
            name: "policy",
            pattern: r"\bRatePolicy\s*(?:::\s*new\s*\(|\{)",
        },
        Shape {
            name: "limiter",
            pattern: r"RateLimiter::new\(|\bQuota::\w+\s*\(|\bgovernor::",
        },
        Shape {
            name: "gate",
            pattern: r"\bRateGate\b",
        },
        Shape {
            name: "semaphore",
            pattern: r"Semaphore::(?:new|const_new)\s*\(",
        },
        Shape {
            name: "constant",
            pattern: r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const|static(?:\s+mut)?)\s+\w*(?:_PER_SEC|_PER_SECOND|_PER_MINUTE|_SPACING|_RATE_LIMIT|_MIN_GAP)\b",
        },
        Shape {
            name: "renamed",
            pattern: r"\b(?:Semaphore|RateLimiter|RatePolicy|RateGate|Quota)\s+as\s+\w+|\btype\s+\w+\s*=\s*(?:\w+::)*(?:Semaphore|RateLimiter|RatePolicy|RateGate|Quota)\b",
        },
    ],
    allowed: &[
        Allow {
            path: "providers/src/limiter.rs",
            shapes: &["policy", "limiter"],
            why: "the limiter itself: a bucket built from a table row, and the one-a-second \
                  fallback for an executor whose row is missing",
        },
        Allow {
            path: "kernel/src/runtime_config/sections/musicbrainz.rs",
            shapes: &["constant"],
            why: "the MusicBrainz rates Settings offers and checks; the kernel sits below the \
                  providers crate and cannot read its table, so \
                  `kernel_musicbrainz_rates_match_the_table` holds them equal",
        },
        Allow {
            path: "auth/src/session/middleware.rs",
            shapes: &["limiter"],
            why: LOGIN_LIMITER,
        },
        Allow {
            path: "auth/src/wiring.rs",
            shapes: &["limiter"],
            why: LOGIN_LIMITER,
        },
        Allow {
            path: "auth/src/routes/native.rs",
            shapes: &["limiter"],
            why: LOGIN_LIMITER,
        },
        Allow {
            path: "auth/src/routes/federated.rs",
            shapes: &["limiter"],
            why: LOGIN_LIMITER,
        },
        Allow {
            path: "plugins/src/wiring.rs",
            shapes: &["limiter"],
            why: "the limiter on inbound plugin routes, not an outside service",
        },
        Allow {
            path: "plugins/src/capabilities/events.rs",
            shapes: &["constant"],
            why: "how many events a plugin may publish here, not an outside service",
        },
        Allow {
            path: "plugins/src/process.rs",
            shapes: &["semaphore"],
            why: LOCAL,
        },
        Allow {
            path: "src/bootstrap/jobs.rs",
            shapes: &["semaphore"],
            why: "the scripted job kind's gate, which holds a run until a test releases it",
        },
        Allow {
            path: "catalog/src/reads/platform/artwork/mod.rs",
            shapes: &["semaphore"],
            why: LOCAL,
        },
        Allow {
            path: "catalog/src/reads/discover/listening/mod.rs",
            shapes: &["semaphore"],
            why: LOCAL,
        },
        Allow {
            path: "acquire/src/slskd/repository.rs",
            shapes: &["semaphore"],
            why: "slskd runs one search and one enqueue at a time; a LAN service is never paced",
        },
        Allow {
            path: "library/src/scan/pool.rs",
            shapes: &["semaphore"],
            why: LOCAL,
        },
        Allow {
            path: "media/src/remotes/connections.rs",
            shapes: &["semaphore"],
            why: "how many reads people trigger run at once against one media server, a memory \
                  bound on capped answers, not a rate",
        },
        Allow {
            path: "kernel/src/db/cache/mod.rs",
            shapes: &["semaphore"],
            why: LOCAL,
        },
        Allow {
            path: "auth/src/passwords.rs",
            shapes: &["semaphore"],
            why: LOCAL,
        },
        Allow {
            path: "acquire/src/requests/service/edition.rs",
            shapes: &["constant"],
            why: UNIT,
        },
        Allow {
            path: "compat/src/jellyfin/seams.rs",
            shapes: &["constant"],
            why: UNIT,
        },
    ],
};

const USER_RUN: &str = "a service the user runs: one factory-built client, never paced";
const PLUGIN_STREAM: &str =
    "opens the stream URL an enabled plugin returned; a plugin is trusted code (PLUGINS.md)";
const TO_MEDIA_SERVERS: &str =
    "passes the factory's client to the user's media servers through `ConnectionResolver`";

const HTTP: Rule = Rule {
    what: "an outbound HTTP client outside the executor and the user-run clients",
    shapes: &[
        Shape {
            name: "client",
            pattern: r"\breqwest::(?:Client|ClientBuilder|RequestBuilder|Request|get|blocking)\b|(?s)\buse\s+reqwest::(?:\*|\{[^}]*\b(?:Client|ClientBuilder|RequestBuilder|Request|get|blocking)\b)",
        },
        Shape {
            name: "renamed",
            pattern: r"\breqwest(?:::\w+)*\s+as\s+\w+|\bextern\s+crate\s+reqwest\b",
        },
        Shape {
            name: "factory",
            pattern: r"\bHttpClientFactory\b|\.\s*(?:no_redirect|remote_audio|direct_no_redirect)\(\)|\b\w*(?:http|factory)\w*\s*\.\s*shared\(\)",
        },
        Shape {
            name: "send",
            pattern: r"(?s)\.\s*(?:get|post|put|patch|delete|head|request)\([^;]*?\.\s*send\(\)",
        },
    ],
    allowed: &[
        Allow {
            path: "providers/src/executor.rs",
            shapes: &["client", "factory"],
            why: "the production wire, the one place a provider request meets reqwest and the \
                  kernel's HTTP clients",
        },
        Allow {
            path: "kernel/src/http_client.rs",
            shapes: &["client", "factory"],
            why: "the HTTP client factory itself",
        },
        Allow {
            path: "kernel/src/platform.rs",
            shapes: &["factory"],
            why: "the platform carries the one factory to the areas",
        },
        Allow {
            path: "acquire/src/clients.rs",
            shapes: &["client"],
            why: "passes the factory's client to slskd, SABnzbd, Prowlarr and Lidarr",
        },
        Allow {
            path: "acquire/src/wiring.rs",
            shapes: &["factory"],
            why: "passes the factory's client to slskd, SABnzbd, Prowlarr and Lidarr",
        },
        Allow {
            path: "auth/src/wiring.rs",
            shapes: &["factory"],
            why: "passes the factory's client to the user's Jellyfin for sign-in",
        },
        Allow {
            path: "catalog/src/reads/mod.rs",
            shapes: &["client"],
            why: TO_MEDIA_SERVERS,
        },
        Allow {
            path: "catalog/src/reads/discover/wiring.rs",
            shapes: &["client"],
            why: TO_MEDIA_SERVERS,
        },
        Allow {
            path: "catalog/src/reads/discover/listening/live.rs",
            shapes: &["client"],
            why: TO_MEDIA_SERVERS,
        },
        Allow {
            path: "media/src/lib.rs",
            shapes: &["client", "factory"],
            why: "passes the factory's clients to the user's media servers through \
                  `ConnectionResolver` and to the plugin stream opener",
        },
        Allow {
            path: "media/src/playback/reports.rs",
            shapes: &["client"],
            why: TO_MEDIA_SERVERS,
        },
        Allow {
            path: "media/src/stream/gateway.rs",
            shapes: &["client"],
            why: PLUGIN_STREAM,
        },
        Allow {
            path: "media/src/stream/plugin_url.rs",
            shapes: &["client"],
            why: PLUGIN_STREAM,
        },
        Allow {
            path: "src/settings/verify.rs",
            shapes: &["client"],
            why: "passes the factory's client to the Test probes of user-run services and \
                  media servers",
        },
        Allow {
            path: "src/settings/wiring.rs",
            shapes: &["factory"],
            why: "passes the factory's client to the settings Test probes",
        },
        Allow {
            path: "src/bootstrap/",
            shapes: &["factory"],
            why: "bootstrap opens the factory and hands its clients to the areas",
        },
        Allow {
            path: "media/src/remotes/",
            shapes: &["client"],
            why: "the user's media servers, each client built by `ConnectionResolver`",
        },
        Allow {
            path: "acquire/src/slskd/",
            shapes: &["client"],
            why: USER_RUN,
        },
        Allow {
            path: "acquire/src/usenet/sabnzbd.rs",
            shapes: &["client", "send"],
            why: USER_RUN,
        },
        Allow {
            path: "acquire/src/usenet/prowlarr.rs",
            shapes: &["client", "send"],
            why: USER_RUN,
        },
        Allow {
            path: "acquire/src/imports/lidarr.rs",
            shapes: &["client", "send"],
            why: USER_RUN,
        },
        Allow {
            path: "auth/src/federated/jellyfin_http.rs",
            shapes: &["client", "send"],
            why: "the user's own Jellyfin, which signs people in",
        },
        Allow {
            path: "plugins/src/capabilities/stream.rs",
            shapes: &["client"],
            why: PLUGIN_STREAM,
        },
    ],
};

const PRIORITY: Rule = Rule {
    what: "a request priority nobody chose",
    shapes: &[
        Shape {
            name: "default",
            pattern: r"impl\s+Default\s+for\s+(?:\w+::)*RequestPriority\b|(?s)enum\s+RequestPriority\s*\{[^}]*#\[default\]",
        },
        Shape {
            name: "alias",
            pattern: r"\bRequestPriority\s+as\s+\w+|\btype\s+\w+\s*=\s*(?:\w+::)*RequestPriority\b",
        },
        Shape {
            name: "setter",
            pattern: r"\bfn\s+with_priority\b",
        },
        Shape {
            name: "constructor",
            pattern: r"(?s)\bfn\s+\w+\b[^{;]*->\s*(?:Self|\w+<\s*Self\b[^{;]*)\s*\{[^}]*?\bpriority\s*(?::\s*(?:\w+::)*RequestPriority::\w+|(?::[^=;]*)?=\s*(?:\w+::)*RequestPriority::\w+)|(?s)\bfn\s+(?:new|default|builder)\b[^{;]*\{[^}]*?\bpriority:\s*(?:\w+::)*RequestPriority::\w+",
        },
        Shape {
            name: "fallback",
            pattern: r"(?:unwrap_or|map_or)(?:_else)?\(\s*(?:\|[^|]*\|\s*)?(?:\w+::)*RequestPriority::",
        },
    ],
    allowed: &[Allow {
        path: "catalog/src/reads/discover/listening/mod.rs",
        shapes: &["constructor"],
        why: "the constructor's name states the caller's choice: a person waiting",
    }],
};

const RULES: &[Rule] = &[RATE, HTTP, PRIORITY];

impl Allow {
    fn covers(&self, path: &str) -> bool {
        if self.path.ends_with('/') {
            path.starts_with(self.path)
        } else {
            path == self.path
        }
    }
}

/// What `rule` refuses in `files` (path and shipping code), and for each
/// allow entry which of its shapes were found.
fn check(rule: &Rule, files: &[(&str, &str)]) -> (Vec<String>, Vec<Vec<bool>>) {
    let mut wrong = Vec::new();
    let mut used: Vec<Vec<bool>> = rule
        .allowed
        .iter()
        .map(|allow| vec![false; allow.shapes.len()])
        .collect();
    for shape in rule.shapes {
        let pattern = Regex::new(shape.pattern).unwrap();
        for (path, code) in files {
            for found in pattern.find_iter(code) {
                let granted = rule.allowed.iter().enumerate().find_map(|(at, allow)| {
                    let slot = allow.shapes.iter().position(|name| *name == shape.name)?;
                    allow.covers(path).then_some((at, slot))
                });
                match granted {
                    Some((at, slot)) => used[at][slot] = true,
                    None => {
                        let line = code[..found.start()].matches('\n').count() + 1;
                        let text = found.as_str().split_whitespace().collect::<Vec<_>>();
                        wrong.push(format!(
                            "{path}:{line}: {} ({}, {})",
                            text.join(" "),
                            rule.what,
                            shape.name
                        ));
                    }
                }
            }
        }
    }
    (wrong, used)
}

#[test]
fn providers_keep_their_rates_wire_and_priorities() {
    let sources = workspace_sources();
    let codes: Vec<String> = sources
        .iter()
        .map(|source| source.shipping_code())
        .collect();
    let files: Vec<(&str, &str)> = sources
        .iter()
        .zip(&codes)
        .map(|(source, code)| (source.path.as_str(), code.as_str()))
        .collect();
    let mut wrong = Vec::new();
    for rule in RULES {
        for allow in rule.allowed {
            for name in allow.shapes {
                assert!(
                    rule.shapes.iter().any(|shape| shape.name == *name),
                    "{} names no shape {name}",
                    allow.path
                );
            }
        }
        let (found, used) = check(rule, &files);
        wrong.extend(found);
        for (allow, used) in rule.allowed.iter().zip(used) {
            for (name, used) in allow.shapes.iter().zip(used) {
                if !used {
                    wrong.push(format!(
                        "{}: listed for {} ({name}), but nothing there is one; remove it ({})",
                        allow.path, rule.what, allow.why
                    ));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "write the rate in the providers' rate table, send through the executor or a \
         user-run service's client, and have every caller name its priority:\n{}",
        wrong.join("\n")
    );
}

/// The kernel cannot read the providers' rate table, so its MusicBrainz
/// rates are held equal to the rows here.
#[test]
fn kernel_musicbrainz_rates_match_the_table() {
    use droppedneedle_kernel::runtime_config::sections::{
        BRAINZMASH_RATE_LIMIT, OFFICIAL_MB_RATE_LIMIT,
    };
    use droppedneedle_providers::rates::{Rate, row};

    let fixed = |source: &str| row(source).map(|row| row.per_second);
    assert_eq!(
        fixed("brainzmash"),
        Some(Rate::Fixed(BRAINZMASH_RATE_LIMIT))
    );
    assert_eq!(
        fixed("musicbrainz"),
        Some(Rate::Fixed(OFFICIAL_MB_RATE_LIMIT))
    );
}

/// Each shape a review found dodging the gate, written in a file no entry
/// lists, fails it.
#[test]
fn dodges_fail_the_gate() {
    let cases: &[(&Rule, &str)] = &[
        (&RATE, "const FETCH_PER_SECOND: f64 = 2.0;"),
        (&RATE, "pub(crate) const CALLS_PER_MINUTE: u32 = 30;"),
        (
            &RATE,
            "const LOOKUP_MIN_GAP: Duration = Duration::from_millis(500);",
        ),
        (&RATE, "static FETCH_RATE_LIMIT: f64 = 2.0;"),
        (&RATE, "static GATE: Semaphore = Semaphore::const_new(1);"),
        (&RATE, "use tokio::sync::Semaphore as Gate;"),
        (&RATE, "type Gate = tokio::sync::Semaphore;"),
        (&RATE, "use droppedneedle_providers::RateLimiter as Bucket;"),
        (
            &RATE,
            "let pace = RatePolicy { per_second: 5.0, burst: 1 };",
        ),
        (&RATE, "let quota = Quota::per_second(nonzero!(5u32));"),
        (&HTTP, "struct Fetch { http: reqwest::Client }"),
        (&HTTP, "use reqwest::{Client, Url};"),
        (&HTTP, "use reqwest::Client as Http;"),
        (&HTTP, "let client = factory.no_redirect().clone();"),
        (&HTTP, "let client = platform.http.shared().clone();"),
        (
            &HTTP,
            "fn new(factory: &HttpClientFactory) -> Self { todo!() }",
        ),
        (
            &HTTP,
            "let answer = self.http.get(&url).timeout(LIMIT).send().await;",
        ),
        (
            &PRIORITY,
            "use droppedneedle_providers::RequestPriority as P;",
        ),
        (&PRIORITY, "type Priority = RequestPriority;"),
        (
            &PRIORITY,
            "fn open(http: Http) -> Self { Self { http, priority: RequestPriority::UserInitiated } }",
        ),
        (
            &PRIORITY,
            "fn open(http: Http) -> Self { let priority = RequestPriority::BackgroundSync; Self { http, priority } }",
        ),
        (
            &PRIORITY,
            "fn open(http: Http) -> Result<Self, Error> { Ok(Self { http, priority: RequestPriority::UserInitiated }) }",
        ),
        (
            &PRIORITY,
            "let priority = asked.map_or(RequestPriority::BackgroundSync, |p| p);",
        ),
        (
            &PRIORITY,
            "let priority = asked.unwrap_or_else(|| RequestPriority::BackgroundSync);",
        ),
    ];
    for (rule, code) in cases {
        let (wrong, _) = check(rule, &[("catalog/src/reads/fetch.rs", code)]);
        assert!(!wrong.is_empty(), "the gate let through: {code}");
    }
}

/// An entry grants only the shapes it names: a semaphore file that grows a
/// rate constant, or a user-run client that reaches the factory, fails.
#[test]
fn entries_grant_only_their_shapes() {
    let cases: &[(&Rule, &str, &str)] = &[
        (
            &RATE,
            "library/src/scan/pool.rs",
            "const SCAN_PER_SECOND: f64 = 4.0;",
        ),
        (
            &HTTP,
            "acquire/src/imports/lidarr.rs",
            "let http = platform.http.shared().clone();",
        ),
    ];
    for (rule, path, code) in cases {
        let (wrong, _) = check(rule, &[(path, code)]);
        assert!(!wrong.is_empty(), "{path} was let through: {code}");
    }
}

/// What the rules leave alone: a body read, a parsed address, a status, a
/// caller that names its priority.
#[test]
fn chosen_shapes_pass() {
    let cases: &[(&Rule, &str)] = &[
        (
            &HTTP,
            "let body = read_capped(response, MAX).await?; let url = reqwest::Url::parse(raw)?;",
        ),
        (
            &HTTP,
            "fn status(answer: &reqwest::Response) -> reqwest::StatusCode { answer.status() }",
        ),
        (&HTTP, "let flight = async move { 1 }.boxed().shared();"),
        (
            &PRIORITY,
            "fn lookup(&self) -> Lookup { self.client.read(RequestPriority::UserInitiated) }",
        ),
        (
            &RATE,
            "const POLL_INTERVAL: Duration = Duration::from_secs(5);",
        ),
    ];
    for (rule, code) in cases {
        let (wrong, _) = check(rule, &[("catalog/src/reads/fetch.rs", code)]);
        assert!(wrong.is_empty(), "refused {code}: {wrong:?}");
    }
}
