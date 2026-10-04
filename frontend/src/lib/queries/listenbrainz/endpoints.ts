import { v3 } from '$lib/api/v3/endpoint';

// v3 ListenBrainz/scrobble URLs, built through the typed registry: every
// template is a literal the contract-coverage gate verifies against the
// generated spec, and hooks import from here so a route rename touches this
// file only.
export const LISTENBRAINZ_ENDPOINTS = {
	connection: v3('/api/v3/settings/listenbrainz'),
	verify: v3('/api/v3/settings/listenbrainz/verify'),
	scrobble: v3('/api/v3/settings/scrobble')
} as const;
