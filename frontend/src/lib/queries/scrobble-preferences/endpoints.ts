import { v3 } from '$lib/api/v3/endpoint';

export const SCROBBLE_PREFERENCES_ENDPOINTS = {
	get: () => v3('/api/v3/me/scrobble-preferences'),
	update: () => v3('/api/v3/me/scrobble-preferences'),
	// v3 personal-mix rows, built through the typed registry.
	personalMixApprovals: () => v3('/api/v3/requests/personal-mix-approvals'),
	approvePersonalMix: (userId: string) =>
		v3('/api/v3/requests/personal-mix-approvals/{user_id}/approve', {
			path: { user_id: userId }
		}),
	rejectPersonalMix: (userId: string) =>
		v3('/api/v3/requests/personal-mix-approvals/{user_id}/reject', {
			path: { user_id: userId }
		}),
	refreshPersonalMixV3: () => v3('/api/v3/requests/personal-mix/refresh')
} as const;
