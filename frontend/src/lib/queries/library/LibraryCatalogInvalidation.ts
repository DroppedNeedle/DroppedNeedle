import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { ArtistQueryKeyFactory } from '$lib/queries/artist/ArtistQueryKeyFactory';
import { DiscoverQueryKeyFactory } from '$lib/queries/discover/DiscoverQueryKeyFactory';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { LyricsQueryKeyFactory } from '$lib/queries/lyrics/LyricsQueryKeyFactory';
import { LOCAL_KEYS } from '$lib/queries/local/LocalV3Keys';
import { RatingQueryKeyFactory } from '$lib/queries/ratings/RatingQueryKeyFactory';
import { SearchQueryKeyFactory } from '$lib/queries/search/SearchQueryKeyFactory';
import { authStore } from '$lib/stores/authStore.svelte';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { everyScope } from '$lib/queries/keys';

// The cross-domain contract for catalog changes, including a landed download:
// every key nested under these prefixes refreshes (the library prefix covers
// the catalog, membership, review and operation keys).
export async function invalidateLibraryCatalog(): Promise<void> {
	await Promise.all([
		invalidateQueriesWithPersister(everyScope(LibraryQueryKeyFactory.feature)),
		invalidateQueriesWithPersister(everyScope(ArtistQueryKeyFactory.feature)),
		invalidateQueriesWithPersister(everyScope(HomeQueryKeyFactory.feature)),
		invalidateQueriesWithPersister(everyScope(DiscoverQueryKeyFactory.feature)),
		invalidateQueriesWithPersister(everyScope('library', 'artist-reconciliation')),
		invalidateQueriesWithPersister(everyScope(LyricsQueryKeyFactory.feature)),
		invalidateQueriesWithPersister(everyScope(LOCAL_KEYS.feature)),
		invalidateQueriesWithPersister(everyScope(RatingQueryKeyFactory.feature)),
		invalidateQueriesWithPersister({ queryKey: SearchQueryKeyFactory.v3.root(authStore.user?.id) })
	]);
}
