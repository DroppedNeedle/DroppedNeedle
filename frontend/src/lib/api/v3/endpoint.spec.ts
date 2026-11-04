import { describe, expect, it } from 'vitest';
import { v3 } from './endpoint';

describe('v3 endpoint builder', () => {
	it('builds a URL string from the template, path and query params', () => {
		const cases: [string, string][] = [
			[v3('/api/v3/library/scan/runs/{id}', { path: { id: 'job 1' } }), '/api/v3/library/scan/runs/job%201'],
			[v3('/api/v3/acquire/slskd/status'), '/api/v3/acquire/slskd/status'],
			[
				v3('/api/v3/requests/history', { query: { status: 'a b', page_size: 5 } }),
				'/api/v3/requests/history?status=a%20b&page_size=5'
			]
		];
		for (const [url, expected] of cases) {
			expect(typeof url).toBe('string');
			expect(url).toBe(expected);
		}
	});

	it('refuses a missing path param and a template the contract does not have', () => {
		// @ts-expect-error - id is required by the template
		expect(() => v3('/api/v3/library/scan/runs/{id}', {})).toThrow(/id/);
		// @ts-expect-error - not a generated route
		v3('/api/v3/nope');
	});
});
