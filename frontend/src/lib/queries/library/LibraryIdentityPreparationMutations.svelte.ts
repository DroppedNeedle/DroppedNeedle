import { createMutation } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { toastStore } from '$lib/stores/toast';
import { searchStore } from '$lib/stores/search';
import { ArtistQueryKeyFactory } from '$lib/queries/artist/ArtistQueryKeyFactory';
import { DiscoverQueryKeyFactory } from '$lib/queries/discover/DiscoverQueryKeyFactory';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { createUuid } from '$lib/utils/uuid';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import type { OperationResponse } from './LibraryOperationsTypes';

async function invalidateIdentityPreparation(
	userId: string | undefined,
	catalogChanged = false
): Promise<void> {
	if (catalogChanged) searchStore.clear();
	await Promise.all([
		invalidateQueriesWithPersister({
			queryKey: LibraryQueryKeyFactory.identityPreparationsPrefix(userId)
		}),
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.activityPrefix() }),
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

export function createLibraryIdentityPreparation(getUserId: () => string | undefined) {
	return createMutation(() => ({
		mutationFn: (rootIds: string[]) =>
			api.global.post<OperationResponse>(API.library.identityPreparations(), {
				idempotency_key: createUuid(),
				root_ids: rootIds
			}),
		onSuccess: async () => {
			await invalidateIdentityPreparation(getUserId());
			toastStore.show({ message: 'Identity preparation started', type: 'success' });
		},
		onError: () =>
			toastStore.show({ message: 'Could not start identity preparation', type: 'error' })
	}));
}

export function applyLibraryIdentityPreparation(getUserId: () => string | undefined) {
	return createMutation(() => ({
		mutationFn: (input: { jobId: string; expectedRevision: number }) =>
			api.global.post<OperationResponse>(API.library.applyIdentityPreparation(input.jobId), {
				expected_row_revision: input.expectedRevision,
				confirmation: true
			}),
		onSuccess: async () => {
			await invalidateIdentityPreparation(getUserId(), true);
			toastStore.show({ message: 'Exact-release mappings accepted', type: 'success' });
		},
		onError: () =>
			toastStore.show({ message: 'Could not accept the identity mappings', type: 'error' })
	}));
}

export function discardLibraryIdentityPreparation(getUserId: () => string | undefined) {
	return createMutation(() => ({
		mutationFn: (input: { jobId: string; expectedRevision: number }) =>
			api.global.post<OperationResponse>(API.library.discardIdentityPreparation(input.jobId), {
				expected_row_revision: input.expectedRevision
			}),
		onSuccess: async () => {
			await invalidateIdentityPreparation(getUserId());
			toastStore.show({ message: 'Identity report dismissed', type: 'success' });
		},
		onError: () =>
			toastStore.show({ message: 'Could not dismiss the identity report', type: 'error' })
	}));
}
