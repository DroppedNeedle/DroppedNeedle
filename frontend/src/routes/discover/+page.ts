import { getDiscoverHomeV3QueryOptions } from '$lib/queries/discover/DiscoverV3Queries.svelte';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import type { PageLoad } from './$types';

export const load: PageLoad = () => {
	void queryClient.prefetchQuery(getDiscoverHomeV3QueryOptions(authStore.user?.id));
	return {};
};
