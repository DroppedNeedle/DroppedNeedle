import type { DiscoverResponse } from '$lib/types';

/**
 * True if a discover response has any renderable section.
 *
 * Shared by the page (to decide whether to show the "Building..." state) and the
 * query layer (client-side stale-while-revalidate: never replace good cached
 * recommendations with an empty "still building" response).
 */
export function discoverHasContent(d: DiscoverResponse | null | undefined): boolean {
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
