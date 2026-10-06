import { v3 } from '$lib/api/v3/endpoint';

// v3 admin settings URLs, one per settings section. Download-client,
// library and library-management sections keep their own feature modules.
export const SETTINGS_ENDPOINTS = {
	advanced: () => v3('/api/v3/settings/advanced'),
	cacheTtls: () => v3('/api/v3/settings/cache-ttls'),
	events: () => v3('/api/v3/settings/events'),
	eventsTestTicketmaster: () => v3('/api/v3/settings/events/test-ticketmaster'),
	eventsTestSkiddle: () => v3('/api/v3/settings/events/test-skiddle'),
	freeMusic: () => v3('/api/v3/settings/free-music'),
	getIt: () => v3('/api/v3/settings/get-it'),
	jellyfin: () => v3('/api/v3/settings/jellyfin'),
	jellyfinVerify: () => v3('/api/v3/settings/jellyfin/verify'),
	lastfm: () => v3('/api/v3/settings/lastfm'),
	navidrome: () => v3('/api/v3/settings/navidrome'),
	navidromeVerify: () => v3('/api/v3/settings/navidrome/verify'),
	navidromePlaylistSync: () => v3('/api/v3/settings/navidrome/playlist-sync'),
	oidc: () => v3('/api/v3/settings/oidc'),
	oidcVerify: () => v3('/api/v3/settings/oidc/verify'),
	preferences: () => v3('/api/v3/settings/preferences'),
	primarySource: () => v3('/api/v3/settings/primary-source'),
	security: () => v3('/api/v3/settings/security'),
	securityVerifyHibp: () => v3('/api/v3/settings/security/verify-hibp'),
	wrapped: () => v3('/api/v3/settings/wrapped'),
	youtube: () => v3('/api/v3/settings/youtube'),
	youtubeVerify: () => v3('/api/v3/settings/youtube/verify')
} as const;

// Server-wide cache maintenance (admin).
export const ADMIN_CACHE_ENDPOINTS = {
	stats: () => v3('/api/v3/admin/cache/stats'),
	clear: () => v3('/api/v3/admin/cache/clear')
} as const;
