/**
 * Audio stream path for one gateway source. The backend route is a wildcard
 * (/stream/{source}/{*key} in server/src/stream/routes.rs) while the spec
 * spells a single-segment {key}, because OpenAPI cannot express the
 * wildcard. Plex part keys carry their own slashes, so the key travels raw
 * and the typed builder's path encoding would corrupt it; this one path is
 * built by hand here in the transport, outside the contract-coverage gate.
 */
export function streamPath(source: string, key: string, search: URLSearchParams): string {
	const query = search.toString();
	return `/api/v3/stream/${source}/${key}${query ? `?${query}` : ''}`;
}
