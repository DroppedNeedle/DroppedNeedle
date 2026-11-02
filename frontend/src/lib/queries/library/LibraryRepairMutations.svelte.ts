import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { toastStore } from '$lib/stores/toast';
import { authStore } from '$lib/stores/authStore.svelte';
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
			await api.v3.POST(v3('/api/v3/library/identity-repairs'), {
				idempotency_key: createUuid(),
				root_ids: rootIds,
				target_matcher_version: 'quiet-reconfirm-v1'
			}),
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
			await api.v3.POST(
				v3('/api/v3/library/identity-repairs/{job_id}/apply', { path: { job_id: input.jobId } }),
				{
					expected_row_revision: input.expectedRevision,
					confirmation: true
				}
			),
		effects: () => repairs(true),
		onError: () => toastStore.show({ message: 'Could not apply the repair', type: 'error' })
	}));
}
