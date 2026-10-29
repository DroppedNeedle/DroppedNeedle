import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { ArtistQueryKeyFactory } from '$lib/queries/artist/ArtistQueryKeyFactory';
import { DiscoverQueryKeyFactory } from '$lib/queries/discover/DiscoverQueryKeyFactory';
import { DownloadQueryKeyFactory } from '$lib/queries/downloads/DownloadQueryKeyFactory';
import { GenreQueryKeyFactory } from '$lib/queries/genre/GenreQueryKeyFactory';
import { DropImportQueryKeyFactory } from '$lib/queries/import/DropImportQueryKeyFactory';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import { LOCAL_KEYS } from '$lib/queries/local/LocalV3Keys';
import { LyricsQueryKeyFactory } from '$lib/queries/lyrics/LyricsQueryKeyFactory';
import { SearchQueryKeyFactory } from '$lib/queries/search/SearchQueryKeyFactory';
import { authStore } from '$lib/stores/authStore.svelte';

import { everyScope } from '$lib/queries/keys';

export async function invalidateLibraryManagementSurfaces(): Promise<void> {
	await Promise.all([
		invalidateQueriesWithPersister(everyScope('library-management')),
		invalidateQueriesWithPersister(everyScope(LibraryQueryKeyFactory.feature, 'operations')),
		invalidateQueriesWithPersister(everyScope(LibraryQueryKeyFactory.feature)),
		invalidateQueriesWithPersister(everyScope(LOCAL_KEYS.feature)),
		invalidateQueriesWithPersister(everyScope(GenreQueryKeyFactory.feature)),
		invalidateQueriesWithPersister(everyScope(ArtistQueryKeyFactory.feature)),
		invalidateQueriesWithPersister(everyScope(HomeQueryKeyFactory.feature)),
		invalidateQueriesWithPersister(everyScope(DiscoverQueryKeyFactory.feature)),
		invalidateQueriesWithPersister(everyScope(DownloadQueryKeyFactory.feature)),
		invalidateQueriesWithPersister(everyScope(DropImportQueryKeyFactory.feature)),
		invalidateQueriesWithPersister(everyScope(LyricsQueryKeyFactory.feature)),
		invalidateQueriesWithPersister({ queryKey: SearchQueryKeyFactory.v3.root(authStore.user?.id) })
	]);
}
