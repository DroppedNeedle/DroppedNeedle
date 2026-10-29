import { api } from '$lib/api/client';
import { toastStore } from '$lib/stores/toast';
import { authStore } from '$lib/stores/authStore.svelte';
import { createUuid } from '$lib/utils/uuid';
import { albumEditionChanged } from '$lib/queries/albums/EditionQueries.svelte';
import { LibraryV3Api } from './LibraryV3Api';
import { toOperationResponse } from './libraryOperationAdapters';
import {
	anyLibraryActivityChanged,
	catalogChanged,
	createAppMutation,
	type Effect,
	identityPreparationsChanged
} from '$lib/queries/effects';

function identityPreparation(userId: string | undefined, catalog = false): Effect[] {
	return [
		identityPreparationsChanged(userId),
		anyLibraryActivityChanged(),
		...(catalog ? [catalogChanged(authStore.user?.id)] : [])
	];
}

export function createLibraryIdentityPreparation(getUserId: () => string | undefined) {
	return createAppMutation(() => ({
		mutationFn: async (rootIds: string[]) =>
			toOperationResponse(
				await api.v3.POST(LibraryV3Api.identityPreparations(), {
					idempotency_key: createUuid(),
					root_ids: rootIds
				})
			),
		effects: () => identityPreparation(getUserId()),
		onSuccess: () => {
			toastStore.show({ message: 'Identity preparation started', type: 'success' });
		},
		onError: () =>
			toastStore.show({ message: 'Could not start identity preparation', type: 'error' })
	}));
}

export function applyLibraryIdentityPreparation(getUserId: () => string | undefined) {
	return createAppMutation(() => ({
		mutationFn: async (input: { jobId: string; expectedRevision: number }) =>
			toOperationResponse(
				await api.v3.POST(LibraryV3Api.applyIdentityPreparation(input.jobId), {
					expected_row_revision: input.expectedRevision,
					confirmation: true
				})
			),
		effects: () => identityPreparation(getUserId(), true),
		onSuccess: () => {
			toastStore.show({ message: 'Exact-release mappings accepted', type: 'success' });
		},
		onError: () =>
			toastStore.show({ message: 'Could not accept the identity mappings', type: 'error' })
	}));
}

export function discardLibraryIdentityPreparation(getUserId: () => string | undefined) {
	return createAppMutation(() => ({
		mutationFn: async (input: { jobId: string; expectedRevision: number }) =>
			toOperationResponse(
				await api.v3.POST(LibraryV3Api.discardIdentityPreparation(input.jobId), {
					expected_row_revision: input.expectedRevision
				})
			),
		effects: () => identityPreparation(getUserId()),
		onSuccess: () => {
			toastStore.show({ message: 'Identity report dismissed', type: 'success' });
		},
		onError: () =>
			toastStore.show({ message: 'Could not dismiss the identity report', type: 'error' })
	}));
}

export function undoLibraryAutomaticEdition(getUserId: () => string | undefined) {
	return createAppMutation(() => ({
		mutationFn: (input: {
			albumId: string;
			expectedAlbumRevision: number;
			expectedIdentityRevision: number;
		}) =>
			api.v3.POST(LibraryV3Api.undoAutomaticEdition(input.albumId), {
				expected_album_revision: input.expectedAlbumRevision,
				expected_identity_revision: input.expectedIdentityRevision
			}),
		effects: (_data, input) => [
			...albumEditionChanged(getUserId(), input.albumId),
			...identityPreparation(getUserId(), true)
		],
		onSuccess: () => {
			toastStore.show({ message: 'Automatic edition acceptance undone', type: 'success' });
		},
		onError: () =>
			toastStore.show({ message: 'Could not undo the automatic edition', type: 'error' })
	}));
}
