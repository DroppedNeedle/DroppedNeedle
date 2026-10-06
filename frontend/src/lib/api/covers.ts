import { getApiUrl } from './api-utils';
import { v3, type V3Url } from './v3/endpoint';

/** Cover size in pixels (the server serves 250, 500 and 1200) or `original`. */
export type CoverSize = number | 'original';

/** MusicBrainz entity a cover belongs to. */
export type CoverKind = 'artist' | 'release-group' | 'release';

/** Typed v3 cover URLs; pass them through `coverSrc` for an `<img>` src. */
export const COVER_ENDPOINTS = {
	artist: (artistId: string, size: CoverSize = 500) =>
		v3('/api/v3/covers/artist/{artist_id}', {
			path: { artist_id: artistId },
			query: { size }
		}),
	releaseGroup: (releaseGroupId: string, size: CoverSize = 500) =>
		v3('/api/v3/covers/release-group/{release_group_id}', {
			path: { release_group_id: releaseGroupId },
			query: { size }
		}),
	release: (releaseId: string, size: CoverSize = 500) =>
		v3('/api/v3/covers/release/{release_id}', {
			path: { release_id: releaseId },
			query: { size }
		})
} as const;

function coverEndpoint(kind: CoverKind, id: string, size: CoverSize): V3Url {
	switch (kind) {
		case 'artist':
			return COVER_ENDPOINTS.artist(id, size);
		case 'release':
			return COVER_ENDPOINTS.release(id, size);
		default:
			return COVER_ENDPOINTS.releaseGroup(id, size);
	}
}

/**
 * Root-relative cover path for one MusicBrainz id. Use it where a component
 * takes a path and prefixes the base itself (BaseImage `customUrl`).
 */
export function coverPath(kind: CoverKind, id: string, size: CoverSize = 500): string {
	return coverEndpoint(kind, id, size);
}

/** Absolute cover URL (base path and PUBLIC_API_URL applied) for an `<img>` src. */
export function coverSrc(kind: CoverKind, id: string, size: CoverSize = 500): string {
	return getApiUrl(coverEndpoint(kind, id, size));
}
