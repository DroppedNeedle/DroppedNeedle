import { describe, expect, it } from 'vitest';

import { PLEX_ENDPOINTS } from './endpoints';

describe('PLEX_ENDPOINTS', () => {
	it('starts every purpose behind one route with a purpose segment', () => {
		expect(PLEX_ENDPOINTS.start('login')).toBe('/api/v3/auth/plex/start?purpose=login');
		expect(PLEX_ENDPOINTS.start('link')).toBe('/api/v3/auth/plex/start?purpose=link');
		expect(PLEX_ENDPOINTS.start('connect')).toBe('/api/v3/auth/plex/start?purpose=connect');
	});

	it('polls each purpose on its own route', () => {
		expect(PLEX_ENDPOINTS.poll('login')).toBe('/api/v3/auth/plex/poll/login');
		expect(PLEX_ENDPOINTS.poll('link')).toBe('/api/v3/auth/plex/poll/link');
		expect(PLEX_ENDPOINTS.poll('connect')).toBe('/api/v3/auth/plex/poll/connect');
	});

	it('builds the settings rows through the same builder shape', () => {
		expect(PLEX_ENDPOINTS.settings()).toBe('/api/v3/settings/plex');
		expect(PLEX_ENDPOINTS.verify()).toBe('/api/v3/settings/plex/verify');
		expect(PLEX_ENDPOINTS.libraries()).toBe('/api/v3/settings/plex/libraries');
	});
});
