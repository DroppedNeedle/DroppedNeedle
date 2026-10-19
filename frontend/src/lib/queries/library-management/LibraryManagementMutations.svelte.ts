import { createMutation } from '@tanstack/svelte-query';

import { ApiError, api } from '$lib/api/client';
import { LIBRARY_MANAGEMENT_ENDPOINTS } from './endpoints';
import { toPreviewDetail } from './libraryManagementAdapters';
import type { OperationResponse } from '$lib/queries/library/LibraryOperationsTypes';
import { operationControlUrl } from '$lib/queries/library/LibraryOperationMutations.svelte';
import { toOperationResponse } from '$lib/queries/library/libraryOperationAdapters';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { toastStore } from '$lib/stores/toast';

import { invalidateLibraryManagementSurfaces } from './LibraryManagementInvalidation';
import { LibraryManagementQueryKeyFactory } from './LibraryManagementQueryKeyFactory';
import type {
	LibraryManagementActivationConfirmRequest,
	LibraryManagementActivationPreviewRequest,
	LibraryManagementApplyRequest,
	LibraryManagementBaselinePurgeRequest,
	LibraryManagementBaselineRestorePreviewRequest,
	LibraryManagementDuplicateResolutionPreviewRequest,
	LibraryManagementDiscardRequest,
	LibraryManagementPreviewCreateRequest,
	LibraryManagementPreviewCreatedResponse,
	LibraryManagementPreviewDetailResponse,
	LibraryManagementPreviewReissueResponse,
	LibraryManagementProfileCopyRequest,
	LibraryManagementProfileDeleteRequest,
	LibraryManagementProfileExportRequest,
	LibraryManagementProfileImportPreviewRequest,
	LibraryManagementProfileImportRequest,
	LibraryManagementSettingsImpactRequest,
	LibraryManagementSettingsUpdateRequest,
	LibraryManagementTagEditPreviewRequest,
	LibraryManagementUndoPreviewRequest
} from './types';

const showActionError = (fallback: string) => (error: Error) => {
	toastStore.show({ message: error.message || fallback, type: 'error' });
};

const showQueued = (message: string) => async () => {
	await invalidateLibraryManagementSurfaces();
	toastStore.show({ message, type: 'success' });
};

export const updateLibraryManagementSettingsMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementSettingsUpdateRequest) =>
			api.global.v3.PUT(LIBRARY_MANAGEMENT_ENDPOINTS.settings(), request),
		onSuccess: invalidateLibraryManagementSurfaces
	}));

export const previewLibraryManagementSettingsImpactMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementSettingsImpactRequest) =>
			api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.impact(), request)
	}));

export const validateLibraryManagementSettingsMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementSettingsImpactRequest) =>
			api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.validate(), request)
	}));

export const copyLibraryManagementProfileMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { profileId: string; request: LibraryManagementProfileCopyRequest }) =>
			api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.copyProfile(input.profileId), input.request),
		onSuccess: invalidateLibraryManagementSurfaces
	}));

export const deleteLibraryManagementProfileMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { profileId: string; request: LibraryManagementProfileDeleteRequest }) =>
			api.global.v3.DELETE(LIBRARY_MANAGEMENT_ENDPOINTS.profile(input.profileId), input.request),
		onSuccess: invalidateLibraryManagementSurfaces
	}));

export const exportLibraryManagementProfileMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { profileId: string; request: LibraryManagementProfileExportRequest }) =>
			api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.exportProfile(input.profileId), input.request)
	}));

export const previewLibraryManagementProfileImportMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementProfileImportPreviewRequest) =>
			api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.profileImportPreview(), request)
	}));

export const importLibraryManagementProfileMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementProfileImportRequest) =>
			api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.profileImports(), request),
		onSuccess: invalidateLibraryManagementSurfaces
	}));

export const createLibraryManagementActivationPreviewMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementActivationPreviewRequest) =>
			api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.activationPreviews(), request),
		onSuccess: async (result) => {
			await invalidateLibraryManagementSurfaces();
			toastStore.show({
				message: result.existing ? 'Active dry run resumed' : 'Activation dry run queued',
				type: 'success'
			});
		},
		onError: showActionError('Could not queue the activation preview')
	}));

export const confirmLibraryManagementActivationMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementActivationConfirmRequest) =>
			api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.activationConfirmations(), request),
		onSuccess: showQueued('File organization activated'),
		onError: showActionError('Could not activate file organization')
	}));

export const createLibraryManagementPreviewMutation = () =>
	createMutation(() => ({
		mutationFn: (
			request: LibraryManagementPreviewCreateRequest
		): Promise<LibraryManagementPreviewCreatedResponse> =>
			api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.previews(), request),
		onSuccess: showQueued('Organization preview queued'),
		onError: showActionError('Could not queue the management preview')
	}));

export const createLibraryManagementTagEditPreviewMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementTagEditPreviewRequest) =>
			api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.tagEditPreviews(), request),
		onSuccess: showQueued('Tag edit preview queued'),
		onError: showActionError('Could not queue the tag edit preview')
	}));

export const applyLibraryManagementPreviewMutation = () =>
	createMutation(() => ({
		mutationFn: async (input: {
			jobId: string;
			request: LibraryManagementApplyRequest;
		}): Promise<OperationResponse> =>
			toOperationResponse(
				await api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.applyPreview(input.jobId), {
					...input.request,
					confirmation: input.request.confirmation ?? false
				})
			),
		onSuccess: showQueued('Organization work queued'),
		onError: showActionError('Could not apply this management preview')
	}));

export const reissueLibraryManagementPreviewMutation = () =>
	createMutation(() => ({
		mutationFn: (input: {
			jobId: string;
			silent?: boolean;
		}): Promise<LibraryManagementPreviewReissueResponse> =>
			api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.reissuePreview(input.jobId)),
		onError: (error: Error, input: { jobId: string; silent?: boolean }) => {
			if (!input.silent) showActionError('Could not resume this management preview')(error);
		}
	}));

export const discardLibraryManagementPreviewMutation = () =>
	createMutation(() => ({
		mutationFn: async (input: {
			jobId: string;
			request: LibraryManagementDiscardRequest;
			silent?: boolean;
		}): Promise<LibraryManagementPreviewDetailResponse> =>
			toPreviewDetail(
				await api.global.v3.POST(
					LIBRARY_MANAGEMENT_ENDPOINTS.discardPreview(input.jobId),
					input.request
				)
			),
		onSuccess: async (
			_response: LibraryManagementPreviewDetailResponse,
			input: { jobId: string; request: LibraryManagementDiscardRequest; silent?: boolean }
		) => {
			if (!input.silent) await showQueued('Organization preview discarded')();
		},
		onError: (
			error: Error,
			input: { jobId: string; request: LibraryManagementDiscardRequest; silent?: boolean }
		) => {
			if (!input.silent) showActionError('Could not discard this management preview')(error);
		}
	}));

export const createLibraryManagementUndoPreviewMutation = () =>
	createMutation(() => ({
		mutationFn: (input: {
			jobId: string;
			request: LibraryManagementUndoPreviewRequest;
		}): Promise<LibraryManagementPreviewCreatedResponse> =>
			api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.undoPreview(input.jobId), input.request),
		onSuccess: showQueued('Undo preview queued'),
		onError: showActionError('Could not queue the undo preview')
	}));

export const createLibraryManagementBaselineRestorePreviewMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementBaselineRestorePreviewRequest) =>
			api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.baselineRestorePreviews(), request),
		onSuccess: showQueued('Baseline restore preview queued'),
		onError: showActionError('Could not queue the baseline restore preview')
	}));

export const createLibraryManagementDuplicateResolutionMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementDuplicateResolutionPreviewRequest) =>
			api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.duplicateResolutionPreviews(), request),
		onSuccess: showQueued('Duplicate resolution preview queued'),
		onError: showActionError('Could not queue the duplicate resolution preview')
	}));

export const previewLibraryManagementBaselinePurgeMutation = () =>
	createMutation(() => ({
		mutationFn: () => api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.baselinePurgeImpact())
	}));

export const purgeLibraryManagementBaselinesMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementBaselinePurgeRequest) =>
			api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.purgeBaselines(), request),
		onSuccess: showQueued('Organization baselines purged'),
		onError: showActionError('Could not purge organization baselines')
	}));

export const resolveLibraryManagementImportBundleMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { bundleId: string }) =>
			api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.resolveImportBundle(input.bundleId))
	}));

const HAND_BACK_UNREADABLE = 'HAND_BACK_UNREADABLE';

// The sentence, with what to do, when an album's files could not be read
// for a hand-back; null for any other outcome.
export function handBackRefusalMessage(error: unknown): string | null {
	if (!(error instanceof ApiError) || error.code !== HAND_BACK_UNREADABLE) return null;
	const action = (error.details as { action?: unknown } | null)?.action;
	return typeof action === 'string' ? `${error.message} ${action}` : error.message;
}

// Hands one album the app stepped back from to automatic management again.
// Its files are then retagged and renamed from its edition.
export const handBackLibraryAlbumMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { albumId: string }) =>
			api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.handBackAlbum(input.albumId)),
		onSuccess: invalidateLibraryManagementSurfaces,
		// Unreadable files are a state the page shows beside the button, not a toast.
		onError: (error: Error) => {
			if (handBackRefusalMessage(error) !== null) return;
			showActionError('Could not hand this album back')(error);
		}
	}));

// Hands back every album the app stepped back from. The server works
// through them in the background.
export const handBackAllLibraryAlbumsMutation = () =>
	createMutation(() => ({
		mutationFn: () => api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.handBackAll()),
		onSuccess: invalidateLibraryManagementSurfaces,
		onError: showActionError('Could not hand these albums back')
	}));

export const controlLibraryManagementOperationMutation = (action: 'pause' | 'resume' | 'stop') =>
	createMutation(() => ({
		mutationFn: async (input: {
			jobId: string;
			expectedRevision: number;
		}): Promise<OperationResponse> =>
			toOperationResponse(
				await api.global.v3.POST(operationControlUrl(action, input.jobId), {
					expected_row_revision: input.expectedRevision
				})
			),
		onSuccess: async () => {
			await invalidateLibraryManagementSurfaces();
			toastStore.show({ message: `Organization ${action} requested`, type: 'success' });
		},
		onError: showActionError(`Could not ${action} this organization operation`)
	}));

// Checks one library folder now. The server creates and removes a folder
// at the top of it.
export const checkLibraryManagementPlacementMutation = (
	getUserId: () => string | null | undefined
) =>
	createMutation(() => ({
		mutationFn: (rootId: string) =>
			api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.checkPlacement(rootId)),
		onSuccess: async (result) => {
			await invalidateQueriesWithPersister({
				queryKey: LibraryManagementQueryKeyFactory.placement(getUserId())
			});
			if (!result.answered) {
				toastStore.show({
					message:
						result.mode === 'unknown'
							? 'Could not check this library right now. The server log has the reason.'
							: 'Could not check this library right now. Its last result is kept.',
					type: 'error'
				});
			}
		},
		onError: showActionError('Could not check this library')
	}));
