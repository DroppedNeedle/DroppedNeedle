import { describe, expect, it, vi, beforeEach } from 'vitest';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts)
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { get: vi.fn() } }
}));

import { api } from '$lib/api/client';
import {
	getLibraryAlbumsQueryOptions,
	getLibraryStatsQueryOptions,
	getLibraryAlbumStatusQueryOptions,
	getLibraryScanStatusQuery,
	getLibraryScanScheduleQuery,
	getLibraryUnmatchedQuery
} from './LibraryQueries.svelte';

const mockGet = vi.mocked(api.global.get);

beforeEach(() => {
	vi.clearAllMocks();
	mockGet.mockResolvedValue({});
});

async function callQueryFn(opts: unknown) {
	const queryFn = (opts as { queryFn: (ctx: { signal: AbortSignal }) => Promise<unknown> }).queryFn;
	await queryFn({ signal: new AbortController().signal });
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

	it('scan status query polls /scan/status fast while scanning, lazily when idle', async () => {
		const opts = getLibraryScanStatusQuery() as unknown as Record<string, unknown>;
		const refetchInterval = opts.refetchInterval as (q: {
			state: { data?: { status: string } };
		}) => number | false;
		expect(refetchInterval({ state: { data: { status: 'scanning' } } })).toBe(2000);
		expect(refetchInterval({ state: { data: { status: 'idle' } } })).toBe(15000);
		expect(refetchInterval({ state: { data: undefined } })).toBe(15000);
		await callQueryFn(opts);
		expect(mockGet.mock.calls[0][0]).toBe('/api/v1/library/scan/status');
	});

	it('unmatched query hits /scan/unmatched', async () => {
		const opts = getLibraryUnmatchedQuery() as unknown as Record<string, unknown>;
		await callQueryFn(opts);
		expect(mockGet.mock.calls[0][0]).toBe('/api/v1/library/scan/unmatched');
	});

	it('scan schedule query hits the schedule endpoint', async () => {
		const opts = getLibraryScanScheduleQuery() as unknown as Record<string, unknown>;
		await callQueryFn(opts);
		expect(mockGet.mock.calls[0][0]).toBe('/api/v1/settings/library/schedule');
	});
});
