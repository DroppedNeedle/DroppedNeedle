import {
	getLibraryAlbumDetailQueryOptions,
	getLibraryAlbumStatusQueryOptions
} from '$lib/features/library/queries/LibraryQueries.svelte';
import { queryClient } from '$lib/queries/QueryClient';
import type { PageLoad } from './$types';

// The album status gates the whole page: it says whether the id is a release
// group the library holds (the matched page) or one to read as a local album,
// so start it as soon as the layout knows who is signed in (its key is that
// user's), and the local read right after it when the id is not a held
// release group. MB-keyed provider endpoints (/basic, /tracks) are not
// prefetched: on a miss they trigger upstream MusicBrainz work for albums
// that may turn out local-only.
export const load: PageLoad = async ({ params, parent }) => {
	const { user } = await parent();
	void queryClient
		.fetchQuery(getLibraryAlbumStatusQueryOptions(params.id))
		.then((status) => {
			if (status.album_id === params.id) {
				return queryClient.prefetchQuery(getLibraryAlbumDetailQueryOptions(user?.id, params.id));
			}
		})
		.catch(() => undefined);
	return {
		albumId: params.id
	};
};
