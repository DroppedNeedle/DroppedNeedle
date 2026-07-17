import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { discoverQueueDeck } from '$lib/stores/discoverQueueDeck.svelte';
import { toastStore } from '$lib/stores/toast';
import { createMutation } from '@tanstack/svelte-query';
import { invalidateDiscoverRecommendations } from './DiscoverInvalidation';

export interface IgnoreDiscoveryItem {
	releaseGroupMbid: string;
	artistMbid: string;
	releaseName: string;
	artistName: string;
}

export const getIgnoreDiscoveryMutation = () =>
	createMutation(() => ({
		mutationFn: (item: IgnoreDiscoveryItem) =>
			api.global.post<void>(API.discoverQueueIgnore(), {
				release_group_mbid: item.releaseGroupMbid,
				artist_mbid: item.artistMbid,
				release_name: item.releaseName,
				artist_name: item.artistName
			}),
		onSuccess: async (_data, item) => {
			discoverQueueDeck.removeByMbid(item.releaseGroupMbid);
			toastStore.show({
				message: "We'll show fewer recommendations like this.",
				type: 'info'
			});
			await invalidateDiscoverRecommendations();
		},
		onError: () => toastStore.show({ message: "Couldn't save that preference.", type: 'error' })
	}));
