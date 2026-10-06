import type { PlexPurpose } from './types';
import { v3 } from '$lib/api/v3/endpoint';

// v3 Plex OAuth URLs, built through the typed registry. Start and poll are
// one literal route per purpose (login is public; link and connect need a
// session), because the coverage gate only verifies literal templates. Call
// sites import from here so a route rename touches this file only.
export const PLEX_ENDPOINTS = {
	start: (purpose: PlexPurpose) =>
		purpose === 'login'
			? v3('/api/v3/auth/plex/start')
			: purpose === 'link'
				? v3('/api/v3/auth/plex/start/link')
				: v3('/api/v3/auth/plex/start/connect'),
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
