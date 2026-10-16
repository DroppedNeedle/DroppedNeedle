import {
	getLibraryAlbumDetailQueryOptions,
	getLibraryAlbumStatusQueryOptions
} from '$lib/queries/library/LibraryQueries.svelte';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import type { PageLoad } from './$types';

// The album status gates the whole page: it says whether the id is a release
// group the library holds (the matched page) or one to read as a local album,
// so start it during the layout bootstrap, and the local read right after it
// when the id is not a held release group. MB-keyed provider endpoints
// (/basic, /tracks) are not prefetched: on a miss they trigger upstream
// MusicBrainz work for albums that may turn out local-only.
export const load: PageLoad = ({ params }) => {
	const userId = authStore.user?.id;
	void queryClient
		.fetchQuery(getLibraryAlbumStatusQueryOptions(params.id))
		.then((status) => {
			if (status.album_id === params.id) {
				return queryClient.prefetchQuery(getLibraryAlbumDetailQueryOptions(userId, params.id));
			}
		})
		.catch(() => undefined);
	return {
		albumId: params.id
	};
};
