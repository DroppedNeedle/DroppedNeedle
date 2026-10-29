import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';

import { everyScope } from '$lib/queries/keys';

export const invalidateArtistReconciliation = () =>
	invalidateQueriesWithPersister(everyScope('library', 'artist-reconciliation'));
