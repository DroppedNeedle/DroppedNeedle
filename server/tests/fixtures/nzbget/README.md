# NZBGet answers

Recorded answers from NZBGet 26.3 (the `nzbgetcom/nzbget:v26.3` image) on
2026-11-16, through `POST /jsonrpc` with HTTP basic auth, the way the
client sends them. The server was a throwaway container with a made-up
sign-in and no news server, so nothing downloaded. No answer holds the
sign-in or any personal data. `testkit/src/nzbget.rs` builds its answers
from these files: a queued job from the recorded queue entry, and every
history entry, completed ones included, from the recorded `DELETED/MANUAL`
item with its id, names, status, size and folder replaced. Nothing could
complete without a news server, so no `SUCCESS` item was recorded.

- `version.json`: `version`.
- `config.json`: `config`, cut to the options the client reads (`MainDir`,
  `DestDir`, `InterDir`, `AppendCategoryDir` and the category names and
  folders) from the 146 a fresh install answers. The rest, the sign-ins
  among them, are left out.
- `listgroups.json`: `listgroups` with `[0]`, holding one job added with
  `append` as the client adds one. With no news server it stays
  `DOWNLOADING`.
- `history.json`: `history` with `[false]`, holding a job removed with
  `GroupDelete` (`DELETED/MANUAL`) and one whose NZB did not parse
  (`FAILURE/SCAN`). A job removed with `GroupFinalDelete` leaves no history.
- `append.json`: `append` with the client's ten parameters, answering the
  new job's id.
- `editqueue.json` and `editqueue_gone.json`: `editqueue`
  `GroupFinalDelete` on a queued job, and on an id NZBGet does not hold.
- `rpc_error.json`: the answer to a method NZBGet does not know (HTTP 200
  with an `error` object).

Not stored as files:

- An empty queue and an empty history both answer `"result": []`.
- A wrong sign-in answers HTTP 401 with an empty `text/plain` body and
  `WWW-Authenticate: Basic realm="NZBGet"`.
