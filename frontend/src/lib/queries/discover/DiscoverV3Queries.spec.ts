import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts)
}));
vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: vi.fn(), POST: vi.fn() } } }
}));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-1' } }
}));
vi.mock('$lib/queries/QueryClient', () => ({
	getQueryData: vi.fn(),
	invalidateQueriesWithPersister: vi.fn().mockResolvedValue(undefined)
}));

import { api } from '$lib/api/client';
import { DiscoverQueryKeyFactory } from './DiscoverQueryKeyFactory';
import {
	getDiscoverHomeV3QueryOptions,
	getDiscoverQueueValidateV3QueryOptions,
	getDiscoverRadioV3Query,
	getDiscoveryBatchV3Query,
	getDiscoveryBatchesV3Query
} from './DiscoverV3Queries.svelte';
import { DiscoverV3Api } from './DiscoverV3Api';

beforeEach(() => vi.clearAllMocks());

async function callQueryFn(
	options: { queryFn?: unknown },
	ctx: { signal?: AbortSignal }
): Promise<unknown> {
	const queryFn = options.queryFn as (ctx: { signal?: AbortSignal }) => Promise<unknown>;
	return queryFn(ctx);
}

describe('DiscoverV3Queries', () => {
	it('fetches the home shelves on the user- and source-scoped key', async () => {
		const options = getDiscoverHomeV3QueryOptions('user-1');
		expect(options.queryKey).toEqual(DiscoverQueryKeyFactory.v3.home('user-1'));
		(api.global.v3.GET as ReturnType<typeof vi.fn>).mockResolvedValue({
			refreshing: false,
			section_status: {}
		});
		await callQueryFn(options, { signal: undefined });
		expect(api.global.v3.GET).toHaveBeenCalledWith(
			'/api/v3/discover',
			expect.objectContaining({ timeoutMs: 15_000 })
		);
	});

	it('polls while the server rebuilds and never refetches on tab focus', () => {
		const options = getDiscoverHomeV3QueryOptions('user-1') as unknown as {
			refetchOnWindowFocus: boolean;
			refetchInterval: (query: { state: { data?: { refreshing?: boolean } } }) => number | false;
		};
		expect(options.refetchOnWindowFocus).toBe(false);
		expect(options.refetchInterval({ state: { data: { refreshing: true } } })).toBe(10_000);
		expect(options.refetchInterval({ state: { data: { refreshing: false } } })).toBe(false);
	});

	it('builds radio shelves with a POST carrying the seed', async () => {
		const query = getDiscoverRadioV3Query(() => ({
			seedType: 'artist',
			seedId: 'mbid-1'
		})) as unknown as {
			queryKey: unknown;
			queryFn: (args: { signal?: AbortSignal }) => Promise<unknown>;
			enabled: boolean;
		};
		expect(query.enabled).toBe(true);
		expect(query.queryKey).toEqual(
			DiscoverQueryKeyFactory.v3.radio('user-1', 'artist', 'mbid-1', {})
		);
		await query.queryFn({});
		expect(api.global.v3.POST).toHaveBeenCalledWith(
			'/api/v3/discover/radio',
			{ seed_type: 'artist', seed_id: 'mbid-1' },
			expect.anything()
		);
		const disabled = getDiscoverRadioV3Query(() => ({
			seedType: 'artist',
			seedId: '',
			enabled: true
		})) as unknown as { enabled: boolean };
		expect(disabled.enabled).toBe(false);
	});

	it('lists batches for the user and gates detail on the batch id', async () => {
		const list = getDiscoveryBatchesV3Query() as unknown as {
			queryKey: unknown;
			queryFn: (args: { signal?: AbortSignal }) => Promise<unknown>;
		};
		expect(list.queryKey).toEqual(DiscoverQueryKeyFactory.v3.batches('user-1'));
		await list.queryFn({});
		expect(api.global.v3.GET).toHaveBeenCalledWith('/api/v3/discover/batches', expect.anything());
		const detail = getDiscoveryBatchV3Query(() => 'batch-1') as unknown as {
			enabled: boolean;
		};
		expect(detail.enabled).toBe(true);
		const missing = getDiscoveryBatchV3Query(() => '') as unknown as { enabled: boolean };
		expect(missing.enabled).toBe(false);
	});

	it('validates queue membership with one POST of release-group ids', async () => {
		const options = getDiscoverQueueValidateV3QueryOptions('user-1', ['rg-1', 'rg-2']);
		expect(options.queryKey).toEqual(
			DiscoverQueryKeyFactory.v3.queueValidate('user-1', ['rg-1', 'rg-2'])
		);
		await callQueryFn(options, {});
		expect(api.global.v3.POST).toHaveBeenCalledWith(
			'/api/v3/discover/queue/validate',
			{ release_group_mbids: ['rg-1', 'rg-2'] },
			expect.anything()
		);
		expect(getDiscoverQueueValidateV3QueryOptions('user-1', []).enabled).toBe(false);
	});

	it('builds every v3 discover URL without hand-built strings at call sites', () => {
		expect(DiscoverV3Api.queue(10)).toBe('/api/v3/discover/queue?count=10');
		expect(DiscoverV3Api.queueStatus()).toBe('/api/v3/discover/queue/status');
		expect(DiscoverV3Api.queueEnrich('rg-1')).toBe('/api/v3/discover/queue/enrich/rg-1');
		expect(DiscoverV3Api.ignored()).toBe('/api/v3/discover/queue/ignored');
		expect(DiscoverV3Api.batch('b-1')).toBe('/api/v3/discover/batches/b-1');
		expect(DiscoverV3Api.radioPlan()).toBe('/api/v3/discover/radio/plan');
		expect(DiscoverV3Api.playlistSuggestions()).toBe('/api/v3/discover/playlist-suggestions');
		expect(DiscoverV3Api.albumPreview('A', 'B', 4)).toBe(
			'/api/v3/discover/album-preview?artist=A&album=B&count=4'
		);
		expect(DiscoverV3Api.youtubeQuota()).toBe('/api/v3/discover/queue/youtube-quota');
	});
});
