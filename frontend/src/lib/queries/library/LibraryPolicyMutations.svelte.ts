import { createMutation } from '@tanstack/svelte-query';
import { api, ApiError } from '$lib/api/client';
import { API } from '$lib/constants';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { toastStore } from '$lib/stores/toast';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { LibraryV3Api } from './LibraryV3Api';
import { toTargetLibrarySettings } from './libraryAdapters';
import type {
	LibraryPolicyApplyPreviewResponse,
	LibraryPolicyImpactResponse,
	LibraryRestoreRootsRequest,
	TargetLibrarySettingsResponse,
	TypedLibrarySettings
} from './LibraryOperationsTypes';

async function invalidatePolicies(): Promise<void> {
	await Promise.all([
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.policyPrefix() }),
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.operationsPrefix() }),
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.activityPrefix() })
	]);
}

function serverErrorMessage(error: unknown, fallback: string): string {
	if (error instanceof ApiError && error.message) return error.message;
	return fallback;
}

export function previewLibraryPolicyImpact() {
	return createMutation(() => ({
		mutationFn: (input: {
			settings: TypedLibrarySettings;
			expected_policy_revision: string | null;
		}) => api.global.post<LibraryPolicyImpactResponse>(API.library.policyImpact(), input)
	}));
}

export function saveTargetLibrarySettings() {
	return createMutation(() => ({
		mutationFn: async (input: {
			settings: TypedLibrarySettings;
			expected_policy_revision: string;
		}) => toTargetLibrarySettings(await api.global.v3.PUT(LibraryV3Api.settings(), input)),
		onSuccess: async () => {
			await invalidatePolicies();
			toastStore.show({ message: 'Library policies saved', type: 'success' });
		},
		onError: (error) =>
			toastStore.show({
				message: serverErrorMessage(error, 'Could not save library policies'),
				type: 'error'
			})
	}));
}

export function restoreLibraryRoots() {
	return createMutation(() => ({
		mutationFn: (input: LibraryRestoreRootsRequest) =>
			api.global.post<TargetLibrarySettingsResponse>(API.library.restoreRoots(), input),
		onSuccess: async () => {
			await invalidatePolicies();
			toastStore.show({ message: 'Library roots restored', type: 'success' });
		},
		onError: (error) =>
			toastStore.show({
				message: serverErrorMessage(error, 'Could not restore library roots'),
				type: 'error'
			})
	}));
}

export function previewLibraryPolicyApply() {
	return createMutation(() => ({
		mutationFn: (input: { scope_ids: string[]; expected_policy_revision: string }) =>
			api.global.post<LibraryPolicyApplyPreviewResponse>(API.library.policyApplyPreview(), input)
	}));
}
