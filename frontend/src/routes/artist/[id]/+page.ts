import { getLibraryArtistDetailQueryOptions } from '$lib/queries/library/LibraryQueries.svelte';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import type { PageLoad } from './$types';

// B7: warm the variant-independent library detail key both hero branches wait on.
// Provider-id keys are deliberately NOT prefetched: the canonical-id redirect hop
// would strand speculatively-fetched keys (see b6-b7 plan §4).
export const load: PageLoad = ({ params }) => {
	void queryClient.prefetchQuery(getLibraryArtistDetailQueryOptions(authStore.user?.id, params.id));
	return {
		artistId: params.id
	};
};
