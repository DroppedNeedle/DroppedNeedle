import {
	SEARCH_BUCKET_PAGE_SIZE,
	getSearchBucketV3QueryOptions
} from '$lib/queries/search/SearchV3Queries.svelte';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import type { PageLoad } from './$types';

export const load: PageLoad = ({ url }) => {
	const query = url.searchParams.get('q') || '';
	if (authStore.user?.id && query.trim().length >= 2) {
		void queryClient.prefetchQuery(
			getSearchBucketV3QueryOptions(
				authStore.user.id,
				'albums',
				query.trim(),
				SEARCH_BUCKET_PAGE_SIZE,
				0
			)
		);
	}
	return {
		query
	};
};
