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
	return (
		(d.because_you_listen_to?.length ?? 0) > 0 ||
		d.fresh_releases != null ||
		d.missing_essentials != null ||
		d.rediscover != null ||
		d.artists_you_might_like != null ||
		d.popular_in_your_genres != null ||
		d.globally_trending != null ||
		d.lastfm_weekly_artist_chart != null ||
		d.lastfm_weekly_album_chart != null ||
		d.lastfm_recent_scrobbles != null ||
		(d.genre_list?.items?.length ?? 0) > 0 ||
		(d.daily_mixes?.length ?? 0) > 0 ||
		(d.radio_sections?.length ?? 0) > 0 ||
		d.discover_picks != null ||
		d.unexplored_genres != null
	);
}
