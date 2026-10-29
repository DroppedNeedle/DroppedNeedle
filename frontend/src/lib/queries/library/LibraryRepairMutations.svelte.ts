import { createMutation } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { toastStore } from '$lib/stores/toast';
import { SearchQueryKeyFactory } from '$lib/queries/search/SearchQueryKeyFactory';
import { authStore } from '$lib/stores/authStore.svelte';
import { ArtistQueryKeyFactory } from '$lib/queries/artist/ArtistQueryKeyFactory';
import { DiscoverQueryKeyFactory } from '$lib/queries/discover/DiscoverQueryKeyFactory';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { LibraryV3Api } from './LibraryV3Api';
import { toOperationResponse } from './libraryOperationAdapters';
import { createUuid } from '$lib/utils/uuid';

async function invalidateRepairs(catalogChanged = false): Promise<void> {
	await Promise.all([
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.repairsPrefix() }),
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.activityPrefix() }),
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.reviewsPrefix() }),
		...(catalogChanged
			? [
					invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.all }),
					invalidateQueriesWithPersister({ queryKey: ArtistQueryKeyFactory.prefix }),
					invalidateQueriesWithPersister({ queryKey: HomeQueryKeyFactory.prefix }),
					invalidateQueriesWithPersister({ queryKey: DiscoverQueryKeyFactory.prefix }),
					invalidateQueriesWithPersister({
						queryKey: SearchQueryKeyFactory.v3.root(authStore.user?.id)
					})
				]
			: [])
	]);
}

export function createLibraryRepair() {
	return createMutation(() => ({
		mutationFn: async (rootIds: string[]) =>
			toOperationResponse(
				await api.v3.POST(LibraryV3Api.identityRepairs(), {
					idempotency_key: createUuid(),
					root_ids: rootIds,
					target_matcher_version: 'quiet-reconfirm-v1'
				})
			),
		onSuccess: async () => {
			await invalidateRepairs();
			toastStore.show({ message: 'Identity recheck started', type: 'success' });
		},
		onError: () =>
			toastStore.show({ message: 'Could not start the identity recheck', type: 'error' })
	}));
}

export function applyLibraryRepair() {
	return createMutation(() => ({
		mutationFn: async (input: { jobId: string; expectedRevision: number }) =>
			toOperationResponse(
				await api.v3.POST(LibraryV3Api.applyIdentityRepair(input.jobId), {
					expected_row_revision: input.expectedRevision,
					confirmation: true
				})
			),
		onSuccess: () => invalidateRepairs(true),
		onError: () => toastStore.show({ message: 'Could not apply the repair', type: 'error' })
	}));
}
