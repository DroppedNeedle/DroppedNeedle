import { getApiUrl } from './api-utils';
import { v3, type V3Url } from './v3/endpoint';

/**
 * Cover size in pixels (the server serves 250, 500 and 1200) or `original`.
 * Artist images take pixels only.
 */
export type CoverSize = number | 'original';

/** MusicBrainz entity a cover belongs to. */
export type CoverKind = 'artist' | 'release-group' | 'release';

/** Typed v3 cover URLs; pass them through `coverSrc` for an `<img>` src. */
export const COVER_ENDPOINTS = {
	artist: (artistId: string, size = 500) =>
		v3('/api/v3/covers/artist/{artist_id}', {
			path: { artist_id: artistId },
			query: { size }
		}),
	releaseGroup: (releaseGroupId: string, size: CoverSize = 500) =>
		v3('/api/v3/covers/release-group/{release_group_id}', {
			path: { release_group_id: releaseGroupId },
			query: { size: String(size) }
		}),
	release: (releaseId: string, size: CoverSize = 500) =>
		v3('/api/v3/covers/release/{release_id}', {
			path: { release_id: releaseId },
			query: { size: String(size) }
		})
} as const;

function coverEndpoint(kind: CoverKind, id: string, size: number): V3Url {
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
export function coverPath(kind: CoverKind, id: string, size = 500): string {
	return coverEndpoint(kind, id, size);
}

/**
 * Root-relative path to a library album's own art (folder image or embedded
 * picture), by local album id. `coverVersion` makes a matching version cache
 * for good; without `size` the server sends the full-size image.
 */
export function albumArtworkPath(albumId: string, coverVersion?: number, size?: CoverSize): string {
	return v3('/api/v3/library/albums/{id}/artwork', {
		path: { id: albumId },
		query: { v: coverVersion, size: size === undefined ? undefined : String(size) }
	});
}

/**
 * Root-relative path to a library artist's picture, a mosaic of their own
 * albums' art, by local artist id. For artists with no MusicBrainz link.
 */
export function artistArtworkPath(artistId: string, size?: CoverSize): string {
	return v3('/api/v3/library/artists/{id}/artwork', {
		path: { id: artistId },
		query: { size: size === undefined ? undefined : String(size) }
	});
}

/** Absolute cover URL (base path and PUBLIC_API_URL applied) for an `<img>` src. */
export function coverSrc(kind: CoverKind, id: string, size = 500): string {
	return getApiUrl(coverEndpoint(kind, id, size));
}
