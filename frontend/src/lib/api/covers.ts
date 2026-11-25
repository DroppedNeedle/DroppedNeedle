import { env } from '$env/dynamic/public';
import { getApiUrl } from './api-utils';
import { v3, type V3Url } from './v3/endpoint';

/**
 * Cover size in pixels (the server serves 250, 375, 500 and 1200) or
 * `original`. Artist images take pixels only.
 */
export type CoverSize = number | 'original';

/** MusicBrainz entity a cover belongs to. */
export type CoverKind = 'artist' | 'release-group' | 'release';

/** Typed v3 cover URL; pass it through `coverSrc` for an `<img>` src. */
function coverEndpoint(kind: CoverKind, id: string, size: number): V3Url {
	switch (kind) {
		case 'artist':
			return v3('/api/v3/covers/artist/{artist_id}', {
				path: { artist_id: id },
				query: { size }
			});
		case 'release':
			return v3('/api/v3/covers/release/{release_id}', {
				path: { release_id: id },
				query: { size: String(size) }
			});
		default:
			return v3('/api/v3/covers/release-group/{release_group_id}', {
				path: { release_group_id: id },
				query: { size: String(size) }
			});
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

/** Our own image routes, after any deployment base. */
const OWN_IMAGE_PATH =
	/\/api\/v3\/(?:covers\/(?:release-group|release|artist)\/[^/]+|library\/(?:albums|artists)\/[^/]+\/artwork)$/;

/** The sizes the server keeps a cover at, smallest first. */
const SERVER_SIZES = [250, 375, 500, 1200] as const;

/**
 * A `srcset` for one of our own cover addresses, as `getApiUrl` builds them:
 * the address at each server size below the size it asks for, then the
 * address itself, so the browser picks the smallest that fills the box. A
 * 250 ask may also take 375, where 250 would be blurry on a sharp screen.
 * Undefined for an outside address, a full-size one, or one with nothing
 * smaller to offer.
 */
export function coverSrcset(url: string): string | undefined {
	const apiOrigin = env.PUBLIC_API_URL?.replace(/\/+$/, '');
	const local = apiOrigin && url.startsWith(`${apiOrigin}/`) ? url.slice(apiOrigin.length) : url;
	if (!local.startsWith('/') || local.startsWith('//')) return undefined;
	const [address] = url.split('#');
	const at = address.indexOf('?');
	const head = at === -1 ? address : address.slice(0, at);
	if (!OWN_IMAGE_PATH.test(head)) return undefined;
	const query = new URLSearchParams(at === -1 ? '' : address.slice(at + 1));
	const asked = Number(query.get('size'));
	if (!Number.isInteger(asked) || asked <= 0) return undefined;
	const top = asked === 250 ? 375 : asked;
	const candidates = SERVER_SIZES.filter((size) => size <= top && size !== asked).map((size) => {
		query.set('size', String(size));
		return { size, url: `${head}?${query}` };
	});
	if (candidates.length === 0) return undefined;
	return [...candidates, { size: asked, url }]
		.sort((a, b) => a.size - b.size)
		.map((candidate) => `${candidate.url} ${candidate.size}w`)
		.join(', ');
}
