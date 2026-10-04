import {
	COMBINED_SEARCH_LIMITS,
	getUnifiedSearchV3QueryOptions
} from '$lib/queries/search/SearchV3Queries.svelte';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import type { PageLoad } from './$types';

// B7: warm the unified search under the same gate as the search query
// (authenticated, >= 2 chars) so back-navigation renders from cache.
export const load: PageLoad = ({ url }) => {
	const q = url.searchParams.get('q') ?? '';
	const query = q.trim();
	if (authStore.user?.id && query.length >= 2) {
		void queryClient.prefetchQuery(
			getUnifiedSearchV3QueryOptions(authStore.user.id, query, COMBINED_SEARCH_LIMITS)
		);
	}
	return { query: q };
};
