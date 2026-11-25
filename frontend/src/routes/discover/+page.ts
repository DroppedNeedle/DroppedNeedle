import { getDiscoverHomeV3QueryOptions } from '$lib/features/discover/queries/DiscoverV3Queries.svelte';
import { getWeekQueryOptions } from '$lib/features/discover/queries/week.svelte';
import { queryClient } from '$lib/queries/QueryClient';
import type { PageLoad } from './$types';

// Wait for the layout to know who is signed in, so the prefetch lands under
// that user's keys and never the signed-out scope.
export const load: PageLoad = async ({ parent }) => {
	const { user } = await parent();
	void queryClient.prefetchQuery(getWeekQueryOptions(user?.id));
	void queryClient.prefetchQuery(getDiscoverHomeV3QueryOptions(user?.id));
	return {};
};
