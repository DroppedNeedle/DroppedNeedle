import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { ArtistQueryKeyFactory } from '$lib/queries/artist/ArtistQueryKeyFactory';
import { DiscoverQueryKeyFactory } from '$lib/queries/discover/DiscoverQueryKeyFactory';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { ArtistReconciliationQueryKeyFactory } from '$lib/queries/artist-reconciliation/ArtistReconciliationQueryKeyFactory';
import { LyricsQueryKeyFactory } from '$lib/queries/lyrics/LyricsQueryKeyFactory';
import { LOCAL_KEYS } from '$lib/queries/local/LocalV3Keys';
import { RatingQueryKeyFactory } from '$lib/queries/ratings/RatingQueryKeyFactory';
import { SearchQueryKeyFactory } from '$lib/queries/search/SearchQueryKeyFactory';
import { authStore } from '$lib/stores/authStore.svelte';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';

// The cross-domain contract for catalog changes, including a landed download:
// every key nested under these prefixes refreshes (the library prefix covers
// the catalog, membership, review and operation keys).
export async function invalidateLibraryCatalog(): Promise<void> {
	await Promise.all([
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.all }),
		invalidateQueriesWithPersister({ queryKey: ArtistQueryKeyFactory.prefix }),
		invalidateQueriesWithPersister({ queryKey: HomeQueryKeyFactory.prefix }),
		invalidateQueriesWithPersister({ queryKey: DiscoverQueryKeyFactory.prefix }),
		invalidateQueriesWithPersister({ queryKey: ArtistReconciliationQueryKeyFactory.prefix }),
		invalidateQueriesWithPersister({ queryKey: LyricsQueryKeyFactory.prefix }),
		invalidateQueriesWithPersister({ queryKey: LOCAL_KEYS.root }),
		invalidateQueriesWithPersister({ queryKey: RatingQueryKeyFactory.prefix }),
		invalidateQueriesWithPersister({ queryKey: SearchQueryKeyFactory.v3.root(authStore.user?.id) })
	]);
}
