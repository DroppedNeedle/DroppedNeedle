import { v3 } from '$lib/api/v3/endpoint';

// v3 Library Management settings and profile URLs. Previews, operations,
// recovery and the tag editor have no v3 route yet and stay in the
// waiting-on-backend list (eslint.config.js).
export const LIBRARY_MANAGEMENT_ENDPOINTS = {
	settings: () => v3('/api/v3/settings/library-management'),
	impact: () => v3('/api/v3/settings/library-management/impact'),
	validate: () => v3('/api/v3/settings/library-management/validate'),
	activationHealth: () => v3('/api/v3/settings/library-management/activation-health'),
	profiles: () => v3('/api/v3/settings/library-management/profiles'),
	profile: (profileId: string) =>
		v3('/api/v3/settings/library-management/profiles/{profile_id}', {
			path: { profile_id: profileId }
		}),
	copyProfile: (profileId: string) =>
		v3('/api/v3/settings/library-management/profiles/{profile_id}/copy', {
			path: { profile_id: profileId }
		}),
	exportProfile: (profileId: string) =>
		v3('/api/v3/settings/library-management/profiles/{profile_id}/export', {
			path: { profile_id: profileId }
		}),
	presetDiff: (profileId: string) =>
		v3('/api/v3/settings/library-management/profiles/{profile_id}/preset-diff', {
			path: { profile_id: profileId }
		}),
	profileImportPreview: () => v3('/api/v3/settings/library-management/profile-imports/preview'),
	profileImports: () => v3('/api/v3/settings/library-management/profile-imports')
} as const;
