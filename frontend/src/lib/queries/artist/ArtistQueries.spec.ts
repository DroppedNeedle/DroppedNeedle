import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createInfiniteQuery: vi.fn((factory: () => unknown) => factory()),
	createQuery: vi.fn((factory: () => unknown) => factory()),
	queryOptions: vi.fn((options: unknown) => options)
}));

const authState = vi.hoisted(() => ({ userId: null as string | null }));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: {
		get user() {
			return authState.userId ? { id: authState.userId } : null;
		}
	}
}));

const mockGet = vi.fn();
vi.mock('$lib/api/client', () => ({
	api: { global: { get: (...args: unknown[]) => mockGet(...args) } }
}));

vi.mock('../QueryClient', () => ({ setQueryDataWithPersister: vi.fn() }));

beforeEach(() => {
	authState.userId = null;
	mockGet.mockReset();
});

import { getArtistReleasesInfiniteQuery, getArtistTopSongsQuery } from './ArtistQueries.svelte';

describe('artist release pagination query', () => {
	it('caps warming polls at 30 per artist key, resetting on cool-down or key change (T9)', () => {
		let artistId = 'artist-1';
		const query = getArtistReleasesInfiniteQuery(() => artistId) as unknown as {
			refetchInterval: (q: {
				state: { data?: { pages?: Array<{ warming?: boolean }> } };
			}) => number | false;
		};

		const state = (warming?: boolean) => ({
			state: { data: { pages: [{ warming }] } }
		});

		for (let i = 0; i < 30; i += 1) {
			expect(query.refetchInterval(state(true))).toBe(2_000);
		}
		expect(query.refetchInterval(state(true))).toBe(false);

		// warming:false resets the budget
		expect(query.refetchInterval(state(false))).toBe(false);
		expect(query.refetchInterval(state(true))).toBe(2_000);

		// artist-key change resets the budget
		artistId = 'artist-2';
		for (let i = 0; i < 30; i += 1) {
			expect(query.refetchInterval(state(true))).toBe(2_000);
		}
		expect(query.refetchInterval(state(true))).toBe(false);
	});
});

it('includes the authenticated user in every source-dependent discovery key', () => {
	authState.userId = 'user-a';
	const userA = getArtistTopSongsQuery(() => ({
		artistId: 'artist-1',
		source: 'listenbrainz'
	})) as unknown as { queryKey: readonly unknown[] };

	authState.userId = 'user-b';
	const userB = getArtistTopSongsQuery(() => ({
		artistId: 'artist-1',
		source: 'listenbrainz'
	})) as unknown as { queryKey: readonly unknown[] };

	expect(userA.queryKey).not.toEqual(userB.queryKey);
});
