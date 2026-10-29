import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts),
	keepPreviousData: 'keepPreviousData'
}));
vi.mock('$lib/api/client', () => ({
	api: { v3: { GET: vi.fn() } }
}));

import { LOCAL_KEYS } from './LocalV3Keys';

beforeEach(() => vi.clearAllMocks());

describe('LocalV3Queries', () => {
	it('keeps the card lists shared and the stats and download access per user', () => {
		const keys = [
			LOCAL_KEYS.albums({ limit: 1, sort: 'date_added', order: 'desc' }),
			LOCAL_KEYS.recent(null),
			LOCAL_KEYS.suggestions(16, null),
			LOCAL_KEYS.search('abba'),
			LOCAL_KEYS.decades(),
			LOCAL_KEYS.albumMatch('rg1', {})
		];
		expect(LOCAL_KEYS.downloadAccess('user-a')).not.toEqual(LOCAL_KEYS.downloadAccess('user-b'));
		expect(LOCAL_KEYS.stats('user-a')).not.toEqual(LOCAL_KEYS.stats('user-b'));
		for (const key of keys) expect(key.slice(0, 2)).toEqual(['shared', 'local']);
	});
});
