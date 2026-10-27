import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts),
	keepPreviousData: 'keepPreviousData'
}));
vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: vi.fn() } } }
}));

import { LOCAL_KEYS } from './LocalV3Keys';

beforeEach(() => vi.clearAllMocks());

describe('LocalV3Queries', () => {
	it('nests every shared local key under the local root without a userId segment', () => {
		const keys = [
			LOCAL_KEYS.albums({ limit: 1, sort: 'date_added', order: 'desc' }),
			LOCAL_KEYS.recent(null),
			LOCAL_KEYS.suggestions(16, null),
			LOCAL_KEYS.search('abba'),
			LOCAL_KEYS.decades(),
			LOCAL_KEYS.stats(),
			LOCAL_KEYS.albumMatch('rg1', {})
		];
		expect(LOCAL_KEYS.downloadAccess('user-a')).not.toEqual(LOCAL_KEYS.downloadAccess('user-b'));
		for (const key of keys) {
			expect([...key][0]).toBe('local');
			expect(key).not.toContain('user-a');
		}
	});
});
