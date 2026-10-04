import { describe, expect, it } from 'vitest';
import { v3 } from './endpoint';

describe('v3 endpoint builder', () => {
	it('builds a URL string from the template and path params', () => {
		const url = v3('/api/v3/acquire/spotify/jobs/{id}', { path: { id: 'job 1' } });

		expect(typeof url).toBe('string');
		expect(url).toBe('/api/v3/acquire/spotify/jobs/job%201');
	});

	it('accepts endpoints without params', () => {
		expect(v3('/api/v3/acquire/health')).toBe('/api/v3/acquire/health');
	});

	it('serializes query params', () => {
		const url = v3('/api/v3/acquire/lidarr-import/artists', { query: { q: 'a b', limit: 5 } });

		expect(url).toBe('/api/v3/acquire/lidarr-import/artists?q=a%20b&limit=5');
	});

	it('requires every path param from the template', () => {
		// @ts-expect-error - id is required by the template
		expect(() => v3('/api/v3/acquire/spotify/jobs/{id}', {})).toThrow(/id/);
	});

	it('only accepts generated route templates', () => {
		expect(v3('/api/v3/acquire/health')).toBe('/api/v3/acquire/health');
		// @ts-expect-error - not a generated route
		v3('/api/v3/nope');
	});
});
