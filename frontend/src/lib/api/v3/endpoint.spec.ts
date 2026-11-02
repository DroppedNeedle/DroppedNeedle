import { describe, expect, it } from 'vitest';
import { v3 } from './endpoint';

describe('v3 endpoint builder', () => {
	it('builds a URL string from the template and path params', () => {
		const url = v3('/api/v3/library/scan/runs/{id}', { path: { id: 'job 1' } });

		expect(typeof url).toBe('string');
		expect(url).toBe('/api/v3/library/scan/runs/job%201');
	});

	it('accepts endpoints without params', () => {
		expect(v3('/api/v3/acquire/slskd/status')).toBe('/api/v3/acquire/slskd/status');
	});

	it('serializes query params', () => {
		const url = v3('/api/v3/requests/history', { query: { status: 'a b', page_size: 5 } });

		expect(url).toBe('/api/v3/requests/history?status=a%20b&page_size=5');
	});

	it('requires every path param from the template', () => {
		// @ts-expect-error - id is required by the template
		expect(() => v3('/api/v3/library/scan/runs/{id}', {})).toThrow(/id/);
	});

	it('only accepts generated route templates', () => {
		expect(v3('/api/v3/acquire/slskd/status')).toBe('/api/v3/acquire/slskd/status');
		// @ts-expect-error - not a generated route
		v3('/api/v3/nope');
	});
});
