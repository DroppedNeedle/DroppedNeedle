import {
	COMBINED_SEARCH_LIMITS,
	getUnifiedSearchV3QueryOptions
} from '$lib/features/search/queries/SearchV3Queries.svelte';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import type { PageLoad } from './$types';

// Warm the unified search under the same gate as the search query
// (authenticated, >= 2 chars) so back-navigation renders from cache.
// A link shared to the installed app (the manifest's share target) comes as
// `link`, and its text as `q`; the link is the search.
export const load: PageLoad = ({ url }) => {
	const q = url.searchParams.get('link') || url.searchParams.get('q') || '';
	const query = q.trim();
	if (authStore.user?.id && query.length >= 2) {
		void queryClient.prefetchQuery(
			getUnifiedSearchV3QueryOptions(authStore.user.id, query, COMBINED_SEARCH_LIMITS)
		);
	}
	return { query: q };
};
