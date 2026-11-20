import type { SearchRemoteStatus } from '$lib/features/search/queries/types';

/** Whether `query` holds a link, which search reads as a song link, never as text. */
export function isLinkQuery(query: string): boolean {
	return /(?:^|\s)(?:https?:\/\/|spotify:track:)/i.test(query);
}

export type SearchStatusNotice = {
	message: string;
	className: 'alert-info' | 'alert-warning';
};

export type SearchNoticeBucket = 'artists' | 'albums' | 'tracks';

const RESULT_TYPE: Record<SearchNoticeBucket, string> = {
	artists: 'artist',
	albums: 'album',
	tracks: 'song'
};

// Worst first: the one notice a page shows names the worst status.
const SEVERITY: SearchRemoteStatus[] = ['error', 'timeout', 'partial', 'stale'];

/**
 * The notice for one bucket's status, or for several when `bucket` is null
 * (the message then names no result type).
 */
export function getSearchStatusNotice(
	status: SearchRemoteStatus,
	bucket: SearchNoticeBucket | null,
	includeLibraryMatches = true
): SearchStatusNotice | null {
	if (status === 'ok') return null;

	const resultType = bucket ? `${RESULT_TYPE[bucket]} ` : '';

	if (status === 'stale') {
		const libraryContext = includeLibraryMatches ? ' alongside any matches in your library' : '';
		return {
			message: `MusicBrainz is unavailable, so we're showing cached ${resultType}results${libraryContext}.`,
			className: 'alert-info'
		};
	}

	if (status === 'timeout') {
		const libraryContext = includeLibraryMatches
			? ' Any matches in your library are still shown.'
			: '';
		return {
			message: `MusicBrainz ${resultType}search timed out.${libraryContext}`,
			className: 'alert-warning'
		};
	}

	if (status === 'partial') {
		return {
			message: `Some MusicBrainz ${resultType}results could not be loaded. Available results are shown below.`,
			className: 'alert-warning'
		};
	}

	const libraryContext = includeLibraryMatches
		? ' Any matches in your library are still shown.'
		: '';
	return {
		message: `MusicBrainz ${resultType}search is temporarily unavailable.${libraryContext}`,
		className: 'alert-warning'
	};
}

/**
 * One notice for a page that shows several buckets: the worst status among
 * them, naming the bucket when it is the only one affected. Songs still on
 * their way (`partial` on the tracks bucket while `songsOnTheWay`) need no
 * notice: the page is asking for them again by itself.
 */
export function getCombinedSearchNotice(
	statuses: Record<SearchNoticeBucket, SearchRemoteStatus>,
	songsOnTheWay = false
): SearchStatusNotice | null {
	const shown = (Object.entries(statuses) as [SearchNoticeBucket, SearchRemoteStatus][]).filter(
		([bucket, status]) =>
			status !== 'ok' && !(bucket === 'tracks' && status === 'partial' && songsOnTheWay)
	);
	if (shown.length === 0) return null;
	const worst = SEVERITY.find((status) => shown.some(([, shownStatus]) => shownStatus === status));
	if (!worst) return null;
	return getSearchStatusNotice(worst, shown.length === 1 ? shown[0][0] : null);
}
