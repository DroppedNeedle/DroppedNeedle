import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createInfiniteQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((options: Record<string, unknown>) => options)
}));

vi.mock('$lib/api/client', () => ({
	api: { v3: { GET: vi.fn().mockResolvedValue({}) } }
}));

import { api } from '$lib/api/client';
import { LibraryManagementQueryKeyFactory } from './LibraryManagementQueryKeyFactory';
import {
	getLibraryManagementActivationPreviewQuery,
	getLibraryManagementPlanItemsQuery
} from './LibraryManagementQueries.svelte';

const mockGet = vi.mocked(api.v3.GET) as unknown as Mock;

beforeEach(() => {
	vi.clearAllMocks();
	mockGet.mockResolvedValue({});
});

describe('LibraryManagementQueryKeyFactory', () => {
	it('keys history and plan items by every filter, never by a page cursor', () => {
		const keys = LibraryManagementQueryKeyFactory;
		const first = keys.operations('admin-a', { cursor: 'page-1', state: 'succeeded' });
		expect(first).toEqual(keys.operations('admin-a', { cursor: 'page-2', state: 'succeeded' }));
		for (const other of [
			{ state: 'failed' },
			{ state: 'succeeded', rootId: 'root-1' }
		] as Parameters<typeof keys.operations>[1][]) {
			expect(first).not.toEqual(keys.operations('admin-a', other));
		}

		const filters = { artistId: 'artist-1', changeKind: 'artwork' } as const;
		expect(keys.previewItems('admin-a', 'job-1', filters)).not.toEqual(
			keys.previewItems('admin-a', 'job-1', { ...filters, artistId: 'artist-2' })
		);
		expect(keys.tagEditor('admin-a', 'track-1')).toEqual([
			'user:admin-a',
			'library-management',
			'tag-editor',
			'track-1'
		]);
	});
});

describe('Library Management query endpoints', () => {
	it('sends the plan-item filters, the artist included', async () => {
		mockGet.mockResolvedValue({ items: [], next_cursor: null });
		const options = getLibraryManagementPlanItemsQuery(
			() => 'admin-a',
			() => 'job-1',
			() => ({ artistId: 'artist-1', changeKind: 'artwork' })
		) as unknown as {
			queryFn: (context: { pageParam?: string; signal: AbortSignal }) => Promise<unknown>;
		};
		await options.queryFn({ signal: new AbortController().signal });

		const url = new URL(String(mockGet.mock.calls[0]?.[0]), 'http://app');
		expect(url.pathname).toBe('/api/v3/library/management/previews/job-1/items');
		expect(url.searchParams.get('artist_id')).toBe('artist-1');
		expect(url.searchParams.get('change_kind')).toBe('artwork');
	});

	it('polls an activation preview until it is ready or terminal', () => {
		const options = getLibraryManagementActivationPreviewQuery(
			() => 'admin-a',
			() => 'activation-1'
		) as unknown as {
			refetchInterval: (query: {
				state: {
					data?: {
						state: string;
						ready_for_confirmation: boolean;
						stale: boolean;
						expired: boolean;
					};
				};
			}) => number | false;
		};
		const preview = {
			state: 'planning',
			ready_for_confirmation: false,
			stale: false,
			expired: false
		};

		expect(options.refetchInterval({ state: {} })).toBe(2000);
		expect(options.refetchInterval({ state: { data: preview } })).toBe(2000);
		expect(
			options.refetchInterval({
				state: { data: { ...preview, state: 'ready', ready_for_confirmation: true } }
			})
		).toBe(false);
		expect(options.refetchInterval({ state: { data: { ...preview, state: 'failed' } } })).toBe(
			false
		);
		expect(options.refetchInterval({ state: { data: { ...preview, state: 'stopped' } } })).toBe(
			false
		);
	});
});
