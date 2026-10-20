# GitHub answers

Recorded answers from the releases endpoint that the update check reads, fetched on
2026-10-20 with the app's User-Agent (`DroppedNeedle/3.0.0 ( contact@droppedneedle.com ; +https://github.com/DroppedNeedle/DroppedNeedle )`)
and `accept: application/vnd.github+json`. The API needs no key, and both requests were
unauthenticated. They were sent one at a time, 166 seconds apart, slower than the
72-second interval of the GitHub row in the rate table.

- `releases_list.json`: HTTP 200 for `GET /repos/DroppedNeedle/DroppedNeedle/releases`.
  The 30 releases the project has published, newest first. All of them are published:
  none is a draft or a prerelease.
- `releases_not_found.json`: HTTP 404 for `GET /repos/DroppedNeedle/droppedneedle-no-such-repo-for-fixture/releases`,
  a repository that does not exist.

The bodies are unchanged except re-indented (two spaces), with one kind of change:

- The `body` of four releases, v2.13.0, v2.8.0, v2.7.0 and v2.4.0, is replaced by
  `Release notes trimmed for the fixture.`, because each was over 4 KB. The other
  release notes are kept as answered. They are the project's own text, so no
  third-party text is copied in.

The answers hold no key, token or personal data. The `author` objects hold the public
login and profile links of the accounts that published the releases, and no email
address. The client reads a GitHub failure by its status only, so the 404 answer has
no model: `github_failures_degrade_to_empty_and_are_not_kept` in
`server/tests/server/providers_events.rs` serves it.
