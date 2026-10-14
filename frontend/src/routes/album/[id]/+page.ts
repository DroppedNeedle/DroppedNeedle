import {
	getLibraryAlbumDetailQueryOptions,
	getLibraryAlbumStatusQueryOptions
} from '$lib/queries/library/LibraryQueries.svelte';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import type { PageLoad } from './$types';

// B7: the album detail read gates the whole page (including the canonical-redirect
// hop), so start it during the layout bootstrap rather than after mount.
// A matched album's page finds its local copies through the status read, so warm
// it in the same fire-and-forget wave. MB-keyed provider endpoints (/basic,
// /tracks) deliberately NOT prefetched: on a miss they trigger upstream
// MusicBrainz work for albums that may turn out local-only.
export const load: PageLoad = ({ params }) => {
	const userId = authStore.user?.id;
	void queryClient.prefetchQuery(getLibraryAlbumDetailQueryOptions(userId, params.id));
	void queryClient.prefetchQuery(getLibraryAlbumStatusQueryOptions(params.id));
	return {
		albumId: params.id
	};
};
