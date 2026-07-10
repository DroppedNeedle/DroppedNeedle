# iTunes Search API - live-verified notes (2026-07-10)

Probed live: `GET https://itunes.apple.com/search?term=nirvana+nevermind&entity=album&country=GB&limit=3`

- Keyless, no auth. Documented limit ~20 calls/minute/IP; 429 above it.
- Response: `{"resultCount": N, "results": [...]}` JSON.
- Album results carry `wrapperType: "collection"`, `collectionType: "Album"`,
  `collectionName`, `artistName`, `collectionViewUrl`
  (e.g. `https://music.apple.com/gb/album/...?uo=4`), `collectionPrice` + `currency`.
- `country` selects the storefront (GB URLs come back under `/gb/`); default is US.
- **Ranking is popularity-driven, not relevance-strict**: "nirvana nevermind"
  returned a tribute ("Piano Tribute to Nirvana") and a live album above the
  actual record. The repository therefore fuzzy-matches `artistName` against the
  requested artist (token_set >= 80) before trusting a result.
- Affiliate: an `at=<token>` query parameter attributes the click (Apple
  Services Performance Partners); appended by the Get-it decorator when active.
