import { describe, expect, it, vi, beforeEach, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts),
	keepPreviousData: vi.fn((data: unknown) => data)
}));

vi.mock('$lib/api/client', () => ({
	api: { v3: { GET: vi.fn(), POST: vi.fn() } }
}));

vi.mock('$lib/queries/QueryClient', () => ({
	setQueryDataWithPersister: vi.fn().mockResolvedValue(undefined)
}));

import { api } from '$lib/api/client';
import { getLibraryMembershipQueryOptions } from './LibraryQueries.svelte';

const mockPost = vi.mocked(api.v3.POST) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;

beforeEach(() => {
	vi.clearAllMocks();
	mockPost.mockResolvedValue({});
});

async function callQueryFn(opts: unknown) {
	const queryFn = (opts as { queryFn: (ctx: { signal: AbortSignal }) => Promise<unknown> }).queryFn;
	return queryFn({ signal: new AbortController().signal });
}

describe('library query endpoints', () => {
	it('chunks discographies larger than the membership request limit', async () => {
		const ids = Array.from(
			{ length: 501 },
			(_, index) => `album-${index.toString().padStart(3, '0')}`
		);
		mockPost
			.mockResolvedValueOnce({
				owned_ids: ['album-000'],
				requested_ids: [],
				held_tracks: [{ id: 'album-000', held: 3, expected: 12 }]
			})
			.mockResolvedValueOnce({ owned_ids: [], requested_ids: ['album-500'], held_tracks: [] });

		const result = await callQueryFn(getLibraryMembershipQueryOptions('user-a', ids));

		expect(mockPost).toHaveBeenCalledTimes(2);
		expect((mockPost.mock.calls[0][1] as { album_ids: string[] }).album_ids).toHaveLength(500);
		expect((mockPost.mock.calls[1][1] as { album_ids: string[] }).album_ids).toEqual(['album-500']);
		expect(result).toEqual({
			owned_ids: ['album-000'],
			requested_ids: ['album-500'],
			held_tracks: [{ id: 'album-000', held: 3, expected: 12 }]
		});
	});
});
