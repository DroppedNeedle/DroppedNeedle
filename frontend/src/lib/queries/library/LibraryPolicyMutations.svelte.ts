import { api } from '$lib/api/client';
import { ApiError } from '$lib/api/errors';
import { toastStore } from '$lib/stores/toast';
import { LibraryV3Api } from './LibraryV3Api';
import { toTargetLibrarySettings } from './libraryAdapters';
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

export function previewLibraryPolicyImpact() {
	return createAppMutation(() => ({
		effects: () => [],
		mutationFn: (input: {
			settings: TypedLibrarySettings;
			expected_policy_revision: string | null;
		}) => api.v3.POST(LibraryV3Api.policyImpact(), input)
	}));
}

export function saveTargetLibrarySettings() {
	return createAppMutation(() => ({
		mutationFn: async (input: {
			settings: TypedLibrarySettings;
			expected_policy_revision: string;
		}) => toTargetLibrarySettings(await api.v3.PUT(LibraryV3Api.settings(), input)),
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

export function restoreLibraryRoots() {
	return createAppMutation(() => ({
		mutationFn: async (input: LibraryRestoreRootsRequest) =>
			toTargetLibrarySettings(await api.v3.POST(LibraryV3Api.restoreRoots(), input)),
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

export function previewLibraryPolicyApply() {
	return createAppMutation(() => ({
		effects: () => [],
		mutationFn: (input: { scope_ids: string[]; expected_policy_revision: string }) =>
			api.v3.POST(LibraryV3Api.policyApplyPreview(), input)
	}));
}
