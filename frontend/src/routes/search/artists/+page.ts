import {
	SEARCH_BUCKET_PAGE_SIZE,
	getSearchBucketV3QueryOptions
} from '$lib/features/search/queries/SearchV3Queries.svelte';
import { queryClient } from '$lib/queries/QueryClient';
import type { PageLoad } from './$types';

export const load: PageLoad = async ({ url, parent }) => {
	const query = url.searchParams.get('q') || '';
	const { user } = await parent();
	if (user?.id && query.trim().length >= 2) {
		void queryClient.prefetchQuery(
			getSearchBucketV3QueryOptions(user.id, 'artists', query.trim(), SEARCH_BUCKET_PAGE_SIZE, 0)
		);
	}
	return {
		query
	};
};
