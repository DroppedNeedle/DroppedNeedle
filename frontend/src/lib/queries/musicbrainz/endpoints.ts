import { v3 } from '$lib/api/v3/endpoint';

export const MUSICBRAINZ_ENDPOINTS = {
	settings: () => v3('/api/v3/settings/musicbrainz'),
	verify: () => v3('/api/v3/settings/musicbrainz/verify'),
	activate: () => v3('/api/v3/settings/musicbrainz/activate'),
	brainzMashConsent: () => v3('/api/v3/settings/musicbrainz/brainzmash/consent'),
	brainzMashStage: () => v3('/api/v3/settings/musicbrainz/brainzmash/stage')
} as const;
