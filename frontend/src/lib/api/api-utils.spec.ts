import { describe, it, expect, vi, beforeEach } from 'vitest';

// `base` (from $app/paths) and PUBLIC_API_URL are resolved at module load, so each case
// re-mocks and re-imports to exercise a different reverse-proxy base path.
async function loadGetApiUrl(base: string, env: Record<string, string> = {}) {
	vi.doMock('$app/paths', () => ({ base }));
	vi.doMock('$env/dynamic/public', () => ({ env }));
	return (await import('./api-utils')).getApiUrl;
}

describe('getApiUrl', () => {
	beforeEach(() => {
		vi.resetModules();
	});

	it('returns root-absolute API paths unchanged when no base path is set', async () => {
		const getApiUrl = await loadGetApiUrl('');
		expect(getApiUrl('/api/v1/covers/x')).toBe('/api/v1/covers/x');
	});

	it('prefixes API paths with the base path when set', async () => {
		const getApiUrl = await loadGetApiUrl('/droppedneedle');
		expect(getApiUrl('/api/v1/covers/x')).toBe('/droppedneedle/api/v1/covers/x');
	});

	it('composes the PUBLIC_API_URL dev override with the base path', async () => {
		const getApiUrl = await loadGetApiUrl('/dn', { PUBLIC_API_URL: 'http://api.test/' });
		expect(getApiUrl('/api/v1/covers/x')).toBe('http://api.test/dn/api/v1/covers/x');
	});

	it('leaves non-absolute input untouched', async () => {
		const getApiUrl = await loadGetApiUrl('/dn');
		expect(getApiUrl('already/relative')).toBe('already/relative');
	});
});
