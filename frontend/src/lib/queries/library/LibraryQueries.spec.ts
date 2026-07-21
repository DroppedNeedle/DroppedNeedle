import { describe, expect, it, vi, beforeEach } from 'vitest';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts)
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { get: vi.fn(), post: vi.fn() } }
}));

import { api } from '$lib/api/client';
import {
	getLibraryAlbumsQueryOptions,
	getLibraryStatsQueryOptions,
	getLibraryAlbumStatusQueryOptions,
	getLibraryScanScheduleQuery,
	getLibraryMembershipQueryOptions
} from './LibraryQueries.svelte';

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
		expect(LibraryQueryKeyFactory.stats()[0]).toBe('library');
		expect(LibraryQueryKeyFactory.album('x')[0]).toBe('library');
	});

	it('album browse key encodes page/sort/q/format', () => {
		const key = LibraryQueryKeyFactory.albums(2, 'title', 'foo', 'flac');
		expect(key).toEqual([
			'library',
			'albums',
			{ page: 2, sort: 'title', q: 'foo', format: 'flac' }
		]);
	});

	it('produces distinct keys for different params', () => {
		expect(LibraryQueryKeyFactory.albums(1, 'recent', '', '')).not.toEqual(
			LibraryQueryKeyFactory.albums(2, 'recent', '', '')
		);
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
	it('albums query hits /library/albums with page, sort and filters', async () => {
		const opts = getLibraryAlbumsQueryOptions({
			page: 3,
			sort: 'artist',
			q: 'rad',
			format: 'flac'
		});
		await callQueryFn(opts);
		const url = mockGet.mock.calls[0][0] as string;
		expect(url).toContain('/api/v1/library/albums');
		expect(url).toContain('page=3');
		expect(url).toContain('sort=artist');
		expect(url).toContain('q=rad');
		expect(url).toContain('format=flac');
	});

	it('albums query omits empty q/format', async () => {
		const opts = getLibraryAlbumsQueryOptions({ page: 1, sort: 'recent', q: '', format: '' });
		await callQueryFn(opts);
		const url = mockGet.mock.calls[0][0] as string;
		expect(url).not.toContain('q=');
		expect(url).not.toContain('format=');
	});

	it('stats query hits /library/stats', async () => {
		await callQueryFn(getLibraryStatsQueryOptions());
		expect(mockGet.mock.calls[0][0]).toBe('/api/v1/library/stats');
	});

	it('album status query hits the combined /status endpoint', async () => {
		await callQueryFn(getLibraryAlbumStatusQueryOptions('rg-1'));
		expect(mockGet.mock.calls[0][0]).toBe('/api/v1/library/albums/rg-1/status');
	});

	it('scan schedule query hits the schedule endpoint', async () => {
		const opts = getLibraryScanScheduleQuery() as unknown as Record<string, unknown>;
		await callQueryFn(opts);
		expect(mockGet.mock.calls[0][0]).toBe('/api/v1/settings/library/schedule');
	});

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
