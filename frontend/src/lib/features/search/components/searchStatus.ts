import type { SearchRemoteStatus } from '$lib/types';

export type SearchStatusNotice = {
	message: string;
	className: 'alert-info' | 'alert-warning';
};

export function getSearchStatusNotice(
	status: SearchRemoteStatus,
	bucket: 'artists' | 'albums',
	includeLibraryMatches = true
): SearchStatusNotice | null {
	if (status === 'ok') return null;

	const resultType = bucket === 'artists' ? 'artist' : 'album';

	if (status === 'stale') {
		const libraryContext = includeLibraryMatches ? ' alongside any matches in your library' : '';
		return {
			message: `MusicBrainz is unavailable, so we're showing cached ${resultType} results${libraryContext}.`,
			className: 'alert-info'
		};
	}

	if (status === 'timeout') {
		const libraryContext = includeLibraryMatches
			? ' Any matches in your library are still shown.'
			: '';
		return {
			message: `MusicBrainz ${resultType} search timed out.${libraryContext}`,
			className: 'alert-warning'
		};
	}

	if (status === 'partial') {
		return {
			message: `Some MusicBrainz ${resultType} results could not be loaded. Available results are shown below.`,
			className: 'alert-warning'
		};
	}

	const libraryContext = includeLibraryMatches
		? ' Any matches in your library are still shown.'
		: '';
	return {
		message: `MusicBrainz ${resultType} search is temporarily unavailable.${libraryContext}`,
		className: 'alert-warning'
	};
}
