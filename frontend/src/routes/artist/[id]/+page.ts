import { getLibraryArtistDetailQueryOptions } from '$lib/features/library/queries/LibraryQueries.svelte';
import { queryClient } from '$lib/queries/QueryClient';
import type { PageLoad } from './$types';

// Warm the variant-independent library detail key both hero branches wait on,
// once the layout knows who is signed in, so it lands under that user's key.
// Provider-id keys are deliberately NOT prefetched: the canonical-id redirect hop
// would strand speculatively-fetched keys.
export const load: PageLoad = async ({ params, parent }) => {
	const { user } = await parent();
	void queryClient.prefetchQuery(getLibraryArtistDetailQueryOptions(user?.id, params.id));
	return {
		artistId: params.id
	};
};
