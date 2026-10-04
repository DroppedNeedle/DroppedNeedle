import type { PlexPurpose } from './types';
import { v3 } from '$lib/api/v3/endpoint';

// v3 Plex OAuth URLs, built through the typed registry. Start takes its
// purpose as a query param; the poll leg is one literal route per purpose,
// because the contract spells the three poll routes separately and the
// coverage gate only verifies literal templates. Call sites import from
// here so a route rename touches this file only.
export const PLEX_ENDPOINTS = {
	start: (purpose: PlexPurpose) => v3('/api/v3/auth/plex/start', { query: { purpose } }),
	poll: (purpose: PlexPurpose) =>
		purpose === 'login'
			? v3('/api/v3/auth/plex/poll/login')
			: purpose === 'link'
				? v3('/api/v3/auth/plex/poll/link')
				: v3('/api/v3/auth/plex/poll/connect'),
	settings: () => v3('/api/v3/settings/plex'),
	verify: () => v3('/api/v3/settings/plex/verify'),
	libraries: () => v3('/api/v3/settings/plex/libraries')
} as const;
