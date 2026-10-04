import { beforeEach, describe, expect, it, vi } from 'vitest';

const captured = vi.hoisted(() => ({ current: null as Record<string, unknown> | null }));

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => {
		captured.current = factory();
		return captured.current;
	})
}));
vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { POST: vi.fn(), GET: vi.fn(), DELETE: vi.fn() } } }
}));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-1' } }
}));
vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn().mockResolvedValue(undefined)
}));
vi.mock('$lib/queries/downloads/DownloadQueryKeyFactory', () => ({
	DownloadQueryKeyFactory: {
		tasks: (userId: string | null | undefined) => ['downloads', 'tasks', userId ?? null]
	}
}));
vi.mock('$lib/stores/discoverQueueDeck.svelte', () => ({
	discoverQueueDeck: { removeByMbid: vi.fn() }
}));
vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

import { api } from '$lib/api/client';
import { DownloadQueryKeyFactory } from '$lib/queries/downloads/DownloadQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { discoverQueueDeck } from '$lib/stores/discoverQueueDeck.svelte';
import { toastStore } from '$lib/stores/toast';
import { DiscoverQueryKeyFactory } from './DiscoverQueryKeyFactory';
import { LibraryQueryKeyFactory } from '../library/LibraryQueryKeyFactory';
import {
	createDiscoveryBatchV3,
	getGenerateDiscoverQueueV3Mutation,
	getIgnoreDiscoveryV3Mutation,
	getQueuePreviewV3Mutation,
	getRefreshDiscoverV3Mutation,
	recordDiscoverActivityV3,
	removeDiscoveryBatchV3
} from './DiscoverV3Mutations.svelte';

type Mutation<TVars, TData = unknown> = {
	mutationFn: (vars: TVars) => Promise<TData>;
	onSuccess?: (data: TData, vars: TVars) => Promise<unknown> | unknown;
};

const mutation = <TVars, TData = unknown>() =>
	captured.current as unknown as Mutation<TVars, TData>;

beforeEach(() => vi.clearAllMocks());

describe('DiscoverV3Mutations', () => {
	it('persists an ignore, confirms it, and refreshes home plus the ledger', async () => {
		getIgnoreDiscoveryV3Mutation();
		const item = {
			releaseGroupMbid: 'rg-1',
			artistMbid: 'artist-1',
			releaseName: 'Album',
			artistName: 'Artist'
		};
		await mutation<typeof item, void>().mutationFn(item);
		expect(api.global.v3.POST).toHaveBeenCalledWith('/api/v3/discover/queue/ignore', {
			release_group_mbid: 'rg-1',
			artist_mbid: 'artist-1',
			release_name: 'Album',
			artist_name: 'Artist'
		});
		await mutation<typeof item, void>().onSuccess?.(undefined, item);
		expect(discoverQueueDeck.removeByMbid).toHaveBeenCalledWith('rg-1');
		expect(toastStore.show).toHaveBeenCalledWith({
			message: "We'll show fewer recommendations like this.",
			type: 'info'
		});
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: DiscoverQueryKeyFactory.v3.home('user-1')
		});
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: DiscoverQueryKeyFactory.v3.ignored('user-1')
		});
	});

	it('triggers a refresh and re-reads home so the poll picks it up', async () => {
		getRefreshDiscoverV3Mutation();
		await mutation<Record<string, never>, { status: string }>().mutationFn({});
		expect(api.global.v3.POST).toHaveBeenCalledWith('/api/v3/discover/refresh');
		await mutation<Record<string, never>>().onSuccess?.({}, {});
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: DiscoverQueryKeyFactory.v3.home('user-1')
		});
	});

	it('generates a queue deck and refreshes status plus deck', async () => {
		getGenerateDiscoverQueueV3Mutation();
		await mutation<{ force?: boolean }>().mutationFn({ force: true });
		expect(api.global.v3.POST).toHaveBeenCalledWith('/api/v3/discover/queue/generate', {
			force: true
		});
		await mutation<{ force?: boolean }>().onSuccess?.({} as never, {});
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: DiscoverQueryKeyFactory.v3.queueStatus('user-1')
		});
	});

	it('previews one card without touching the cache', async () => {
		getQueuePreviewV3Mutation();
		const signal = new AbortController().signal;
		await mutation<{ mbid: string; signal: AbortSignal }>().mutationFn({
			mbid: 'rg-1',
			signal
		});
		expect(api.global.v3.POST).toHaveBeenCalledWith('/api/v3/discover/queue/preview/rg-1', undefined, {
			signal
		});
		expect(mutation<{ mbid: string; signal: AbortSignal }>().onSuccess).toBeUndefined();
	});

	it('records activity against v3 without failing the caller', async () => {
		(api.global.v3.POST as ReturnType<typeof vi.fn>).mockResolvedValue({
			generation: 1,
			source_id: 's-1',
			source_mode: 'brainzmash'
		});
		await recordDiscoverActivityV3({ feature: 'discover' });
		expect(api.global.v3.POST).toHaveBeenCalledWith(
			'/api/v3/discover/activity',
			{ feature: 'discover' },
			{ signal: undefined }
		);
	});

	it('creates a batch, toasts the outcome, and sweeps batches plus download tasks', async () => {
		(api.global.v3.POST as ReturnType<typeof vi.fn>).mockResolvedValue({
			id: 'batch-1',
			name: 'Batch',
			item_count: 2,
			imported_count: 0,
			pending_count: 2,
			items: [
				{ release_group_mbid: 'rg-1', outcome: 'requested' },
				{ release_group_mbid: 'rg-2', outcome: 'skipped_in_library' }
			]
		});
		const created = await createDiscoveryBatchV3({ name: 'Batch', items: [] });
		expect(created?.id).toBe('batch-1');
		expect(toastStore.show).toHaveBeenCalledWith({
			message: '1 album requested · 1 already yours or requested',
			type: 'success'
		});
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: DiscoverQueryKeyFactory.v3.batches('user-1')
		});
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: DownloadQueryKeyFactory.tasks('user-1')
		});
	});

	it('removes a batch with albums and sweeps tasks, batches, detail, and library counts', async () => {
		(api.global.v3.DELETE as ReturnType<typeof vi.fn>).mockResolvedValue({
			cancelled_requests: 1,
			removed_albums: 2,
			kept: 0
		});
		const result = await removeDiscoveryBatchV3('batch-1', true);
		expect(result?.removed_albums).toBe(2);
		expect(api.global.v3.DELETE).toHaveBeenCalledWith(
			'/api/v3/discover/batches/batch-1?remove_albums=true'
		);
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: DiscoverQueryKeyFactory.v3.batches('user-1')
		});
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: DiscoverQueryKeyFactory.v3.batch('user-1', 'batch-1')
		});
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: DownloadQueryKeyFactory.tasks('user-1')
		});
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.v3.stats('user-1')
		});
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: [...LibraryQueryKeyFactory.v3.root('user-1'), 'recently-added']
		});
	});

	it('keeps albums on a record-only removal and skips the library sweep', async () => {
		(api.global.v3.DELETE as ReturnType<typeof vi.fn>).mockResolvedValue({
			cancelled_requests: 0,
			removed_albums: 0,
			kept: 0
		});
		await removeDiscoveryBatchV3('batch-1', false);
		expect(toastStore.show).toHaveBeenCalledWith({
			message: 'Batch record removed - albums kept',
			type: 'success'
		});
		expect(invalidateQueriesWithPersister).not.toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.v3.stats('user-1')
		});
		expect(invalidateQueriesWithPersister).not.toHaveBeenCalledWith({
			queryKey: DownloadQueryKeyFactory.tasks('user-1')
		});
	});
});
