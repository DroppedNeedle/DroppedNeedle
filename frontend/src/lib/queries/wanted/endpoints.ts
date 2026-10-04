import { v3 } from '$lib/api/v3/endpoint';

// v3 wanted URLs, built through the typed registry: every template is a
// literal the contract-coverage gate verifies against the generated spec,
// and hooks import from here so a route rename touches this file only.
export const WANTED_ENDPOINTS = {
	list: () => v3('/api/v3/requests/wanted'),
	stop: (mbid: string) =>
		v3('/api/v3/requests/wanted/{musicbrainz_id}/stop', { path: { musicbrainz_id: mbid } }),
	resume: (mbid: string) =>
		v3('/api/v3/requests/wanted/{musicbrainz_id}/resume', { path: { musicbrainz_id: mbid } }),
	seen: (mbid: string) =>
		v3('/api/v3/requests/wanted/{musicbrainz_id}/seen', { path: { musicbrainz_id: mbid } })
} as const;
