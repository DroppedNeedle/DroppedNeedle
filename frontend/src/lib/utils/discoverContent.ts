/**
 * True if a discover response has any renderable section.
 *
 * Shared by the page (to decide whether to show the "Building..." state) and the
 * query layer (client-side stale-while-revalidate: never replace good cached
 * recommendations with an empty "still building" response).
 *
 * Structural on purpose: both the v1 hand-mirrored shape and the generated v3
 * contract satisfy it, so the v1 and v3 home queries share this check.
 */
interface DiscoverShelfLike {
	items?: unknown[];
}

interface DiscoverContentLike {
	because_you_listen_to?: { section?: DiscoverShelfLike | null }[] | null;
	fresh_releases?: DiscoverShelfLike | null;
	missing_essentials?: DiscoverShelfLike | null;
	rediscover?: DiscoverShelfLike | null;
	artists_you_might_like?: DiscoverShelfLike | null;
	popular_in_your_genres?: DiscoverShelfLike | null;
	genre_list?: DiscoverShelfLike | null;
	globally_trending?: DiscoverShelfLike | null;
	weekly_exploration?: { tracks?: unknown[] } | null;
	lastfm_weekly_artist_chart?: DiscoverShelfLike | null;
	lastfm_weekly_album_chart?: DiscoverShelfLike | null;
	lastfm_recent_scrobbles?: DiscoverShelfLike | null;
	daily_mixes?: DiscoverShelfLike[] | null;
	radio_sections?: DiscoverShelfLike[] | null;
	top_picks?: DiscoverShelfLike | null;
	listeners_like_you?: DiscoverShelfLike | null;
	anniversaries?: DiscoverShelfLike | null;
	new_from_followed?: DiscoverShelfLike | null;
	unexplored_genres?: DiscoverShelfLike | null;
}

export function discoverHasContent(d: DiscoverContentLike | null | undefined): boolean {
	if (!d) return false;
	const hasItems = (section: { items?: unknown[] } | null | undefined) =>
		(section?.items?.length ?? 0) > 0;
	return (
		d.because_you_listen_to?.some((entry) => hasItems(entry.section)) ||
		hasItems(d.fresh_releases) ||
		hasItems(d.missing_essentials) ||
		hasItems(d.rediscover) ||
		hasItems(d.artists_you_might_like) ||
		hasItems(d.popular_in_your_genres) ||
		hasItems(d.globally_trending) ||
		hasItems(d.lastfm_weekly_artist_chart) ||
		hasItems(d.lastfm_weekly_album_chart) ||
		hasItems(d.lastfm_recent_scrobbles) ||
		hasItems(d.genre_list) ||
		(d.weekly_exploration?.tracks?.length ?? 0) > 0 ||
		d.daily_mixes?.some(hasItems) ||
		d.radio_sections?.some(hasItems) ||
		(d.top_picks?.items?.length ?? 0) > 0 ||
		hasItems(d.listeners_like_you) ||
		hasItems(d.anniversaries) ||
		hasItems(d.new_from_followed) ||
		hasItems(d.unexplored_genres)
	);
}
