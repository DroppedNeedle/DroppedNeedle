import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { ApiError } from '$lib/api/errors';
import { toastStore } from '$lib/stores/toast';
import type { LibraryRestoreRootsRequest, TypedLibrarySettings } from './LibraryOperationsTypes';
import {
	anyLibraryActivityChanged,
	createAppMutation,
	libraryOperationsChanged,
	libraryPoliciesChanged
} from '$lib/queries/effects';

const policies = () => [
	libraryPoliciesChanged(),
	libraryOperationsChanged(),
	anyLibraryActivityChanged()
];

function serverErrorMessage(error: unknown, fallback: string): string {
	if (error instanceof ApiError && error.message) return error.message;
	return fallback;
}

export function createPreviewLibraryPolicyImpactMutation() {
	return createAppMutation(() => ({
		effects: () => [],
		mutationFn: (input: {
			settings: TypedLibrarySettings;
			expected_policy_revision: string | null;
		}) => api.v3.POST(v3('/api/v3/settings/library/policy-impact'), input)
	}));
}

export function createSaveTargetLibrarySettingsMutation() {
	return createAppMutation(() => ({
		mutationFn: async (input: {
			settings: TypedLibrarySettings;
			expected_policy_revision: string;
		}) => await api.v3.PUT(v3('/api/v3/settings/library'), input),
		effects: policies,
		onSuccess: () => {
			toastStore.show({ message: 'Library policies saved', type: 'success' });
		},
		onError: (error) =>
			toastStore.show({
				message: serverErrorMessage(error, 'Could not save library policies'),
				type: 'error'
			})
	}));
}

export function createRestoreLibraryRootsMutation() {
	return createAppMutation(() => ({
		mutationFn: async (input: LibraryRestoreRootsRequest) =>
			await api.v3.POST(v3('/api/v3/settings/library/restore-roots'), input),
		effects: policies,
		onSuccess: () => {
			toastStore.show({ message: 'Library roots restored', type: 'success' });
		},
		onError: (error) =>
			toastStore.show({
				message: serverErrorMessage(error, 'Could not restore library roots'),
				type: 'error'
			})
	}));
}

export function createPreviewLibraryPolicyApplyMutation() {
	return createAppMutation(() => ({
		effects: () => [],
		mutationFn: (input: { scope_ids: string[]; expected_policy_revision: string }) =>
			api.v3.POST(v3('/api/v3/settings/library/policy-apply-preview'), input)
	}));
}
