import { getHomeQueryOptions } from '$lib/queries/HomeQuery.svelte';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import type { PageLoad } from './$types';

// Warm the home hero query during the layout bootstrap instead of after it.
export const load: PageLoad = () => {
	void queryClient.prefetchQuery(getHomeQueryOptions(authStore.user?.id));
	return {};
};
