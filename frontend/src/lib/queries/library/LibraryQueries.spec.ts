import { describe, expect, it, vi, beforeEach } from 'vitest';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts),
	keepPreviousData: vi.fn((data: unknown) => data)
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { get: vi.fn(), post: vi.fn() } }
}));

vi.mock('../QueryClient', () => ({
	setQueryDataWithPersister: vi.fn().mockResolvedValue(undefined)
}));

import { api } from '$lib/api/client';
import { getLibraryMembershipQueryOptions } from './LibraryQueries.svelte';

const mockGet = vi.mocked(api.global.get);
const mockPost = vi.mocked(api.global.post);

beforeEach(() => {
	vi.clearAllMocks();
	mockGet.mockResolvedValue({});
	mockPost.mockResolvedValue({});
});

async function callQueryFn(opts: unknown) {
	const queryFn = (opts as { queryFn: (ctx: { signal: AbortSignal }) => Promise<unknown> }).queryFn;
	return queryFn({ signal: new AbortController().signal });
}

describe('LibraryQueryKeyFactory', () => {
	it('keys start with the library prefix', () => {
		expect(LibraryQueryKeyFactory.all[0]).toBe('library');
		expect(LibraryQueryKeyFactory.album('x')[0]).toBe('library');
	});

	it('produces distinct keys for different params', () => {
		expect(LibraryQueryKeyFactory.album('a')).not.toEqual(LibraryQueryKeyFactory.album('b'));
	});

	it('keys membership by user and normalized candidate IDs', () => {
		const first = getLibraryMembershipQueryOptions('user-a', ['B', 'a', 'b']);
		const second = getLibraryMembershipQueryOptions('user-b', ['a', 'b']);
		expect(first.queryKey).toEqual(['library', 'membership', 'user-a', ['a', 'b']]);
		expect(first.queryKey).not.toEqual(second.queryKey);
	});
});

describe('library query endpoints', () => {
	it('membership query posts only its bounded candidate set', async () => {
		const opts = getLibraryMembershipQueryOptions('user-a', ['B', 'a', 'b']);
		await callQueryFn(opts);
		expect(mockPost).toHaveBeenCalledWith(
			'/api/v1/library/membership',
			{ album_ids: ['a', 'b'] },
			{ signal: expect.any(AbortSignal) }
		);
	});

	it('chunks discographies larger than the membership request limit', async () => {
		const ids = Array.from(
			{ length: 501 },
			(_, index) => `album-${index.toString().padStart(3, '0')}`
		);
		mockPost
			.mockResolvedValueOnce({ owned_ids: ['album-000'], requested_ids: [] })
			.mockResolvedValueOnce({ owned_ids: [], requested_ids: ['album-500'] });

		const result = await callQueryFn(getLibraryMembershipQueryOptions('user-a', ids));

		expect(mockPost).toHaveBeenCalledTimes(2);
		expect((mockPost.mock.calls[0][1] as { album_ids: string[] }).album_ids).toHaveLength(500);
		expect((mockPost.mock.calls[1][1] as { album_ids: string[] }).album_ids).toEqual(['album-500']);
		expect(result).toEqual({ owned_ids: ['album-000'], requested_ids: ['album-500'] });
	});
});
