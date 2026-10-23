import { isValidMbid } from '$lib/utils/formatting';
import { getApiUrl } from '$lib/api/api-utils';
import { withBasePath } from '$lib/utils/basePath';

export function isAbortError(error: unknown): boolean {
	return (
		(error instanceof DOMException && error.name === 'AbortError') ||
		(error instanceof Error && error.name === 'AbortError')
	);
}

export function getLocalAlbumArtworkUrl(albumId: string): string {
	return getApiUrl(`/api/v1/library/albums/${encodeURIComponent(albumId)}/artwork/local`);
}

/** Recognize explicit local identity only on the application's configured API route. */
export function getLocalArtworkAlbumId(coverUrl: string | null | undefined): string | null {
	const match = coverUrl?.match(/\/api\/v1\/library\/albums\/([^/?#]+)\/artwork\/local$/);
	if (!match) return null;
	let albumId: string;
	try {
		albumId = decodeURIComponent(match[1]);
	} catch {
		return null;
	}
	if (
		!albumId ||
		albumId === '.' ||
		albumId === '..' ||
		albumId.includes('/') ||
		albumId.includes('\\') ||
		[...albumId].some(
			(character) => character.charCodeAt(0) < 32 || character.charCodeAt(0) === 127
		)
	)
		return null;
	const raw = `/api/v1/library/albums/${encodeURIComponent(albumId)}/artwork/local`;
	return coverUrl === raw || coverUrl === withBasePath(raw) || coverUrl === getApiUrl(raw)
		? albumId
		: null;
}

export function getCoverUrl(coverUrl: string | null | undefined, albumId: string): string {
	// Audio source and album identity are independent. Only preserve an explicit,
	// matching local route, including after launcher/player URL normalization.
	if (getLocalArtworkAlbumId(coverUrl) === albumId) return getLocalAlbumArtworkUrl(albumId);
	if (isValidMbid(albumId)) {
		return getApiUrl(`/api/v1/covers/release-group/${albumId}?size=250`);
	}
	return coverUrl
		? getApiUrl(coverUrl)
		: getApiUrl(`/api/v1/covers/release-group/${albumId}?size=250`);
}
