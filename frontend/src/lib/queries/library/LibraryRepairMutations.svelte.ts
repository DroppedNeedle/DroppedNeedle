import { createMutation } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { toastStore } from '$lib/stores/toast';
import { searchStore } from '$lib/stores/search';
import { ArtistQueryKeyFactory } from '$lib/queries/artist/ArtistQueryKeyFactory';
import { DiscoverQueryKeyFactory } from '$lib/queries/discover/DiscoverQueryKeyFactory';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import type { OperationResponse } from './LibraryOperationsTypes';
import { createUuid } from '$lib/utils/uuid';

async function invalidateRepairs(catalogChanged = false): Promise<void> {
	if (catalogChanged) searchStore.clear();
	await Promise.all([
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.repairsPrefix() }),
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.activityPrefix() }),
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.reviewsPrefix() }),
		...(catalogChanged
			? [
					invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.all }),
					invalidateQueriesWithPersister({ queryKey: ArtistQueryKeyFactory.prefix }),
					invalidateQueriesWithPersister({ queryKey: HomeQueryKeyFactory.prefix }),
					invalidateQueriesWithPersister({ queryKey: DiscoverQueryKeyFactory.prefix })
				]
			: [])
	]);
}

export function createLibraryRepair() {
	return createMutation(() => ({
		mutationFn: (rootIds: string[]) =>
			api.global.post<OperationResponse>(API.library.identityRepairs(), {
				idempotency_key: createUuid(),
				root_ids: rootIds,
				target_matcher_version: 'feedback-fixes-v1'
			}),
		onSuccess: async () => {
			await invalidateRepairs();
			toastStore.show({ message: 'Existing-match check started', type: 'success' });
		},
		onError: () =>
			toastStore.show({ message: 'Could not start the existing-match check', type: 'error' })
	}));
}

export function applyLibraryRepair() {
	return createMutation(() => ({
		mutationFn: (input: { jobId: string; expectedRevision: number }) =>
			api.global.post<OperationResponse>(API.library.applyIdentityRepair(input.jobId), {
				expected_row_revision: input.expectedRevision,
				confirmation: true
			}),
		onSuccess: () => invalidateRepairs(true),
		onError: () => toastStore.show({ message: 'Could not apply the repair', type: 'error' })
	}));
}
