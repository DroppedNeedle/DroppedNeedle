/** Auth endpoints. Login/setup happen before a session exists, so these are
 * always called via the unauthenticated `api.global` client. */
export const AUTH_ENDPOINTS = {
	providers: '/api/v1/auth/providers',
	login: '/api/v1/auth/login',
	jellyfinLogin: '/api/v1/auth/jellyfin/login',
	setup: '/api/v1/auth/setup',
	oidcAuthorize: '/api/v1/auth/oidc/authorize',
	oidcExchange: '/api/v1/auth/oidc/exchange',
	plexPin: '/api/v1/auth/plex/pin',
	plexPoll: (pinId: string) => `/api/v1/auth/plex/poll?pin_id=${encodeURIComponent(pinId)}`
} as const;
