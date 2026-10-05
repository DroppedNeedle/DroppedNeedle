import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts),
	keepPreviousData: 'keepPreviousData'
}));
vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: vi.fn() } } }
}));

import { LOCAL_V3_KEYS } from './LocalV3Keys';

beforeEach(() => vi.clearAllMocks());

describe('LocalV3Queries', () => {
	it('nests every v3 local key under the local root without a userId segment', () => {
		const keys = [
			LOCAL_V3_KEYS.albums({ limit: 1, offset: 0, sort: 'recent', order: 'desc' }),
			LOCAL_V3_KEYS.recent(null),
			LOCAL_V3_KEYS.suggestions(16, null),
			LOCAL_V3_KEYS.search('abba', null),
			LOCAL_V3_KEYS.decades(),
			LOCAL_V3_KEYS.stats(),
			LOCAL_V3_KEYS.albumMatch('rg1', {})
		];
		for (const key of keys) {
			expect([...key].slice(0, 2)).toEqual(['local', 'v3']);
		}
	});
});
