import { getHomeQueryOptions } from '$lib/features/home/queries/home.svelte';
import { queryClient } from '$lib/queries/QueryClient';
import type { PageLoad } from './$types';

// Warm the home hero query during the layout bootstrap instead of after it.
// Wait for the layout to know who is signed in, so the prefetch lands under
// that user's keys and never the signed-out scope.
export const load: PageLoad = async ({ parent }) => {
	const { user } = await parent();
	void queryClient.prefetchQuery(getHomeQueryOptions(user?.id));
	return {};
};
