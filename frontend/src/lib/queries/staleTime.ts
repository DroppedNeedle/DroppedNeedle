// How long a query's data counts as fresh. Every query uses one of these
// three; one that needs another value keeps it with the reason beside it.

/** Kept current by server events or a poll, so a mount always refetches. */
export const STALE_LIVE = 0;

/** Everything else. Set once as the QueryClient's default. */
export const STALE_DEFAULT = 60_000;

/** Artist, album, Discover and home: slow to change and costly to rebuild. */
export const STALE_SLOW = 30 * 60_000;
