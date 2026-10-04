import { v3 } from '$lib/api/v3/endpoint';

// v3 self-service profile URLs, built through the typed registry: every
// template is a literal the contract-coverage gate verifies against the
// generated spec, and hooks import from here so a route rename touches this
// file only. The current user is resolved server-side from the session
// cookie, so none of these take a user id.
export const PROFILE_ENDPOINTS = {
	get: () => v3('/api/v3/me'),
	update: () => v3('/api/v3/me'),
	updateUsername: () => v3('/api/v3/me/username'),
	updateEmail: () => v3('/api/v3/me/email'),
	changePassword: () => v3('/api/v3/me/password'),
	setPassword: () => v3('/api/v3/me/local-password'),
	avatarUpload: () => v3('/api/v3/me/avatar'),
	// Plain string by necessity: raw image bytes for an <img> src, not a
	// typed JSON call. No v1 fallback; the old profile avatar route is gone.
	avatar: (userId: string) => `/api/v3/users/${encodeURIComponent(userId)}/avatar`
} as const;
