import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { DiscoverQueryKeyFactory } from './DiscoverQueryKeyFactory';

export const invalidateDiscoverRecommendations = () =>
	invalidateQueriesWithPersister({
		queryKey: DiscoverQueryKeyFactory.v3.root(authStore.user?.id)
	});
