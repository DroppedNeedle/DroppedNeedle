import { coverSrc } from '$lib/api/covers';
import { isValidMbid } from '$lib/utils/formatting';
import { getApiUrl } from '$lib/api/api-utils';

export function isAbortError(error: unknown): boolean {
	return (
		(error instanceof DOMException && error.name === 'AbortError') ||
		(error instanceof Error && error.name === 'AbortError')
	);
}

/** The release group's cover when `albumId` is a MusicBrainz id, else the given one, else none. */
export function getCoverUrl(coverUrl: string | null | undefined, albumId: string): string | null {
	if (isValidMbid(albumId)) return coverSrc('release-group', albumId, 250);
	return coverUrl ? getApiUrl(coverUrl) : null;
}
