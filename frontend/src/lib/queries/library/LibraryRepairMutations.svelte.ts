import { api } from '$lib/api/client';
import { toastStore } from '$lib/stores/toast';
import { authStore } from '$lib/stores/authStore.svelte';
import { LibraryV3Api } from './LibraryV3Api';
import { toOperationResponse } from './libraryOperationAdapters';
import { createUuid } from '$lib/utils/uuid';
import {
	anyLibraryActivityChanged,
	catalogChanged,
	createAppMutation,
	type Effect,
	repairsChanged,
	reviewsChanged
} from '$lib/queries/effects';

function repairs(catalog = false): Effect[] {
	return [
		repairsChanged(),
		anyLibraryActivityChanged(),
		reviewsChanged(),
		...(catalog ? [catalogChanged(authStore.user?.id)] : [])
	];
}

export function createLibraryRepair() {
	return createAppMutation(() => ({
		mutationFn: async (rootIds: string[]) =>
			toOperationResponse(
				await api.v3.POST(LibraryV3Api.identityRepairs(), {
					idempotency_key: createUuid(),
					root_ids: rootIds,
					target_matcher_version: 'quiet-reconfirm-v1'
				})
			),
		effects: () => repairs(),
		onSuccess: () => {
			toastStore.show({ message: 'Identity recheck started', type: 'success' });
		},
		onError: () =>
			toastStore.show({ message: 'Could not start the identity recheck', type: 'error' })
	}));
}

export function applyLibraryRepair() {
	return createAppMutation(() => ({
		mutationFn: async (input: { jobId: string; expectedRevision: number }) =>
			toOperationResponse(
				await api.v3.POST(LibraryV3Api.applyIdentityRepair(input.jobId), {
					expected_row_revision: input.expectedRevision,
					confirmation: true
				})
			),
		effects: () => repairs(true),
		onError: () => toastStore.show({ message: 'Could not apply the repair', type: 'error' })
	}));
}
