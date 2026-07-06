import { API } from '$lib/constants';

export const SCROBBLE_PREFERENCES_ENDPOINTS = {
	get: API.me.scrobblePreferences(),
	update: API.me.scrobblePreferences(),
	refreshPersonalMix: API.me.personalMixRefresh(),
	personalMixApprovals: () => API.requests.personalMixApprovals(),
	approvePersonalMix: (userId: string) => API.requests.approvePersonalMix(userId),
	rejectPersonalMix: (userId: string) => API.requests.rejectPersonalMix(userId)
} as const;
