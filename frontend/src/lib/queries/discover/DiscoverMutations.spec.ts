import { beforeEach, describe, expect, it, vi } from 'vitest';

const captured = vi.hoisted(() => ({ current: null as Record<string, unknown> | null }));

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => {
		captured.current = factory();
		return captured.current;
	})
}));
vi.mock('$lib/api/client', () => ({
	api: { global: { post: vi.fn().mockResolvedValue(undefined) } }
}));
vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn().mockResolvedValue(undefined)
}));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'u1' } }
}));
vi.mock('$lib/stores/discoverQueueDeck.svelte', () => ({
	discoverQueueDeck: { removeByMbid: vi.fn() }
}));
vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { discoverQueueDeck } from '$lib/stores/discoverQueueDeck.svelte';
import { toastStore } from '$lib/stores/toast';
import { getIgnoreDiscoveryMutation } from './DiscoverMutations.svelte';

describe('getIgnoreDiscoveryMutation', () => {
	beforeEach(() => vi.clearAllMocks());

	it('persists the signal, confirms it, and invalidates the user-scoped Discover cache', async () => {
		getIgnoreDiscoveryMutation();
		const item = {
			releaseGroupMbid: 'rg-1',
			artistMbid: 'artist-1',
			releaseName: 'Album',
			artistName: 'Artist'
		};
		const mutation = captured.current as {
			mutationFn: (value: typeof item) => Promise<void>;
			onSuccess: (data: void, value: typeof item) => Promise<void>;
		};

		await mutation.mutationFn(item);
		await mutation.onSuccess(undefined, item);

		expect(api.global.post).toHaveBeenCalledWith(API.discoverQueueIgnore(), {
			release_group_mbid: 'rg-1',
			artist_mbid: 'artist-1',
			release_name: 'Album',
			artist_name: 'Artist'
		});
		expect(toastStore.show).toHaveBeenCalledWith({
			message: "We'll show fewer recommendations like this.",
			type: 'info'
		});
		expect(discoverQueueDeck.removeByMbid).toHaveBeenCalledWith('rg-1');
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: ['discover', 'u1']
		});
	});
});
