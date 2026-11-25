import {
	COMBINED_SEARCH_LIMITS,
	getUnifiedSearchV3QueryOptions
} from '$lib/features/search/queries/SearchV3Queries.svelte';
import { queryClient } from '$lib/queries/QueryClient';
import type { PageLoad } from './$types';

// Warm the unified search under the same gate as the search query
// (authenticated, >= 2 chars) so back-navigation renders from cache. It waits
// for the layout to know who is signed in, so a first load warms it too.
// A link shared to the installed app (the manifest's share target) comes as
// `link`, and its text as `q`; the link is the search.
export const load: PageLoad = async ({ url, parent }) => {
	const q = url.searchParams.get('link') || url.searchParams.get('q') || '';
	const query = q.trim();
	const { user } = await parent();
	if (user?.id && query.length >= 2) {
		void queryClient.prefetchQuery(
			getUnifiedSearchV3QueryOptions(user.id, query, COMBINED_SEARCH_LIMITS)
		);
	}
	return { query: q };
};
