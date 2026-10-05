import { createMutation } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { invalidateLibraryCatalog } from './LibraryCatalogInvalidation';
import { LibraryQueryKeyFactory, type LibraryV3UserId } from './LibraryQueryKeyFactory';
import { LibraryV3Api } from './LibraryV3Api';

export type IdentifyBodyV3 = components['schemas']['IdentifyBody'];
export type IdentifyResponseV3 = components['schemas']['IdentifyResponse'];
export type ManagePreviewBodyV3 = components['schemas']['ManagePreviewBody'];
export type ManagePreviewResponseV3 = components['schemas']['ManagePreviewResponse'];
export type ManageApplyBodyV3 = components['schemas']['ManageApplyBody'];
export type ManageApplyResponseV3 = components['schemas']['ManageApplyResponse'];
export type ManageUndoBodyV3 = components['schemas']['ManageUndoBody'];
export type ManageUndoResponseV3 = components['schemas']['ManageUndoResponse'];
export type BaselineRestoreBodyV3 = components['schemas']['BaselineRestoreBody'];
export type BaselineRestoreResponseV3 = components['schemas']['BaselineRestoreResponse'];
export type ApproveBodyV3 = components['schemas']['ApproveBody'];
export type ReviewResolveResponseV3 = components['schemas']['ReviewResolveResponse'];
export type ScanBodyV3 = components['schemas']['ScanBody'];
export type ScanResponseV3 = components['schemas']['ScanResponse'];
export type AddRootBodyV3 = components['schemas']['AddRootBody'];
export type RootViewV3 = components['schemas']['RootView'];

export interface EditionPinV3Variables {
	userId: LibraryV3UserId;
	albumId: string;
	releaseMbid: string;
}

export interface EditionClearV3Variables {
	userId: LibraryV3UserId;
	albumId: string;
}

function assertLocalAlbumId(albumId: string): void {
	if (!albumId) throw new Error('Missing local album id for the edition pin.');
}

function invalidatePinScope(variables: EditionClearV3Variables): Promise<unknown> {
	return Promise.all([
		invalidateQueriesWithPersister({
			queryKey: LibraryQueryKeyFactory.v3.editionPin(variables.userId, variables.albumId)
		}),
		invalidateQueriesWithPersister({
			queryKey: LibraryQueryKeyFactory.v3.albumDetail(variables.userId, variables.albumId)
		})
	]);
}

export function setLibraryEditionPinV3() {
	return createMutation(() => ({
		mutationFn: (vars: EditionPinV3Variables) => {
			assertLocalAlbumId(vars.albumId);
			return api.global.v3.PUT(LibraryV3Api.editionPin(vars.albumId), {
				release_mbid: vars.releaseMbid
			});
		},
		onSuccess: (_data, vars) => invalidatePinScope(vars)
	}));
}

export function clearLibraryEditionPinV3() {
	return createMutation(() => ({
		mutationFn: (vars: EditionClearV3Variables) => {
			assertLocalAlbumId(vars.albumId);
			return api.global.v3.DELETE(LibraryV3Api.editionPin(vars.albumId));
		},
		onSuccess: (_data, vars) => invalidatePinScope(vars)
	}));
}

export function enqueueLibraryIdentifyV3() {
	return createMutation(() => ({
		mutationFn: (body: IdentifyBodyV3) => api.global.v3.POST(LibraryV3Api.identify(), body),
		onSuccess: async () => {
			await invalidateLibraryCatalog();
			toastStore.show({ message: 'Identification started', type: 'success' });
		},
		onError: () => toastStore.show({ message: 'Could not start identification', type: 'error' })
	}));
}

export function previewLibraryManageV3() {
	return createMutation(() => ({
		// A preview seals a token without changing the catalog, so it leaves
		// the cache alone; the apply below does the sweeping.
		mutationFn: (body: ManagePreviewBodyV3) =>
			api.global.v3.POST(LibraryV3Api.managePreview(), body)
	}));
}

export function applyLibraryManageV3() {
	return createMutation(() => ({
		mutationFn: (body: ManageApplyBodyV3) => api.global.v3.POST(LibraryV3Api.manageApply(), body),
		onSuccess: async () => {
			await invalidateLibraryCatalog();
			toastStore.show({ message: 'Changes published', type: 'success' });
		},
		onError: () =>
			toastStore.show({ message: 'The library changed; preview it again', type: 'error' })
	}));
}

export function undoLibraryManageV3() {
	return createMutation(() => ({
		mutationFn: (body: ManageUndoBodyV3) => api.global.v3.POST(LibraryV3Api.manageUndo(), body),
		onSuccess: async () => {
			await invalidateLibraryCatalog();
			toastStore.show({ message: 'Changes undone', type: 'success' });
		},
		onError: () => toastStore.show({ message: 'Could not undo those changes', type: 'error' })
	}));
}

export function restoreLibraryBaselineV3() {
	return createMutation(() => ({
		mutationFn: (body: BaselineRestoreBodyV3) =>
			api.global.v3.POST(LibraryV3Api.baselineRestore(), body),
		onSuccess: async () => {
			await invalidateLibraryCatalog();
			toastStore.show({ message: 'Original files restored', type: 'success' });
		},
		onError: () =>
			toastStore.show({ message: 'Could not restore the original files', type: 'error' })
	}));
}

export function approveLibraryReviewV3() {
	return createMutation(() => ({
		mutationFn: (vars: { reviewId: string; candidateKey: string }) =>
			api.global.v3.POST(LibraryV3Api.approveReview(vars.reviewId), {
				candidate_key: vars.candidateKey
			}),
		onSuccess: invalidateLibraryCatalog,
		onError: (error) =>
			toastStore.show({
				message: error instanceof Error ? error.message : 'Could not save this album decision',
				type: 'error'
			})
	}));
}

export function rejectLibraryReviewV3() {
	return createMutation(() => ({
		mutationFn: (vars: { reviewId: string }) =>
			api.global.v3.POST(LibraryV3Api.rejectReview(vars.reviewId)),
		onSuccess: invalidateLibraryCatalog,
		onError: (error) =>
			toastStore.show({
				message: error instanceof Error ? error.message : 'Could not save this album decision',
				type: 'error'
			})
	}));
}

export function triggerLibraryScanV3() {
	return createMutation(() => ({
		mutationFn: (body: ScanBodyV3) => api.global.v3.POST(LibraryV3Api.scan(), body),
		onSuccess: async (response: ScanResponseV3) => {
			const userId = authStore.user?.id;
			await invalidateQueriesWithPersister({
				queryKey: LibraryQueryKeyFactory.v3.scanRuns(userId)
			});
			await invalidateQueriesWithPersister({
				queryKey: LibraryQueryKeyFactory.v3.scanRun(userId, response.run_id)
			});
		}
	}));
}

export function addLibraryRootV3() {
	return createMutation(() => ({
		mutationFn: (body: AddRootBodyV3) => api.global.v3.POST(LibraryV3Api.roots(), body),
		onSuccess: async () => {
			await invalidateQueriesWithPersister({
				queryKey: LibraryQueryKeyFactory.v3.roots(authStore.user?.id)
			});
			toastStore.show({ message: 'Library root added', type: 'success' });
		},
		onError: () => toastStore.show({ message: 'Could not add that library root', type: 'error' })
	}));
}
