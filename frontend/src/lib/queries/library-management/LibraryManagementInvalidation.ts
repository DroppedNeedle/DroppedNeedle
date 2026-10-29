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

import { LibraryManagementQueryKeyFactory } from './LibraryManagementQueryKeyFactory';

export async function invalidateLibraryManagementSurfaces(): Promise<void> {
	await Promise.all([
		invalidateQueriesWithPersister({ queryKey: LibraryManagementQueryKeyFactory.prefix }),
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.operationsPrefix() }),
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.all }),
		invalidateQueriesWithPersister({ queryKey: LOCAL_KEYS.root }),
		invalidateQueriesWithPersister({ queryKey: GenreQueryKeyFactory.prefix }),
		invalidateQueriesWithPersister({ queryKey: ArtistQueryKeyFactory.prefix }),
		invalidateQueriesWithPersister({ queryKey: HomeQueryKeyFactory.prefix }),
		invalidateQueriesWithPersister({ queryKey: DiscoverQueryKeyFactory.prefix }),
		invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.all }),
		invalidateQueriesWithPersister({ queryKey: DropImportQueryKeyFactory.prefix }),
		invalidateQueriesWithPersister({ queryKey: LyricsQueryKeyFactory.prefix }),
		invalidateQueriesWithPersister({ queryKey: SearchQueryKeyFactory.v3.root(authStore.user?.id) })
	]);
}
