import { createMutation } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { LIBRARY_MANAGEMENT_ENDPOINTS } from './endpoints';
import type { OperationResponse } from '$lib/queries/library/LibraryOperationsTypes';
import { toastStore } from '$lib/stores/toast';

import { invalidateLibraryManagementSurfaces } from './LibraryManagementInvalidation';
import type {
	LibraryManagementActivationConfirmRequest,
	LibraryManagementActivationPreviewRequest,
	LibraryManagementApplyRequest,
	LibraryManagementBaselinePurgeImpactResponse,
	LibraryManagementBaselinePurgeRequest,
	LibraryManagementBaselinePurgeResponse,
	LibraryManagementBaselineRestorePreviewRequest,
	LibraryManagementDuplicateResolutionPreviewRequest,
	LibraryManagementDiscardRequest,
	LibraryManagementImportBundleResolveResponse,
	LibraryManagementPreviewCreateRequest,
	LibraryManagementPreviewCreatedResponse,
	LibraryManagementPreviewDetailResponse,
	LibraryManagementPreviewReissueResponse,
	LibraryManagementProfileCopyRequest,
	LibraryManagementProfileCreateRequest,
	LibraryManagementProfileDeleteRequest,
	LibraryManagementProfileExportRequest,
	LibraryManagementProfileImportPreviewRequest,
	LibraryManagementProfileImportRequest,
	LibraryManagementProfileUpdateRequest,
	LibraryManagementSettingsImpactRequest,
	LibraryManagementSettingsResponse,
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

export const createLibraryManagementProfileMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementProfileCreateRequest) =>
			api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.profiles(), request),
		onSuccess: invalidateLibraryManagementSurfaces
	}));

export const copyLibraryManagementProfileMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { profileId: string; request: LibraryManagementProfileCopyRequest }) =>
			api.global.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.copyProfile(input.profileId), input.request),
		onSuccess: invalidateLibraryManagementSurfaces
	}));

export const updateLibraryManagementProfileMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { profileId: string; request: LibraryManagementProfileUpdateRequest }) =>
			api.global.v3.PUT(LIBRARY_MANAGEMENT_ENDPOINTS.profile(input.profileId), input.request),
		onSuccess: invalidateLibraryManagementSurfaces
	}));

export const deleteLibraryManagementProfileMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { profileId: string; request: LibraryManagementProfileDeleteRequest }) =>
			// The typed client sends no DELETE body, so this one call names its type.
			api.global.delete<LibraryManagementSettingsResponse>(
				LIBRARY_MANAGEMENT_ENDPOINTS.profile(input.profileId),
				{
					body: input.request
				}
			),
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
			api.global.post<LibraryManagementPreviewCreatedResponse>(
				API.libraryManagement.activationPreviews(),
				request
			),
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
			api.global.post<LibraryManagementSettingsResponse>(
				API.libraryManagement.activationConfirmations(),
				request
			),
		onSuccess: showQueued('File organization activated'),
		onError: showActionError('Could not activate file organization')
	}));

export const createLibraryManagementPreviewMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementPreviewCreateRequest) =>
			api.global.post<LibraryManagementPreviewCreatedResponse>(
				API.libraryManagement.previews(),
				request
			),
		onSuccess: showQueued('Organization preview queued'),
		onError: showActionError('Could not queue the management preview')
	}));

export const createLibraryManagementTagEditPreviewMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementTagEditPreviewRequest) =>
			api.global.post<LibraryManagementPreviewCreatedResponse>(
				API.libraryManagement.tagEditPreviews(),
				request
			),
		onSuccess: showQueued('Tag edit preview queued'),
		onError: showActionError('Could not queue the tag edit preview')
	}));

export const applyLibraryManagementPreviewMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { jobId: string; request: LibraryManagementApplyRequest }) =>
			api.global.post<OperationResponse>(
				API.libraryManagement.applyPreview(input.jobId),
				input.request
			),
		onSuccess: showQueued('Organization work queued'),
		onError: showActionError('Could not apply this management preview')
	}));

export const reissueLibraryManagementPreviewMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { jobId: string; silent?: boolean }) =>
			api.global.post<LibraryManagementPreviewReissueResponse>(
				API.libraryManagement.reissuePreview(input.jobId)
			),
		onError: (error: Error, input: { jobId: string; silent?: boolean }) => {
			if (!input.silent) showActionError('Could not resume this management preview')(error);
		}
	}));

export const discardLibraryManagementPreviewMutation = () =>
	createMutation(() => ({
		mutationFn: (input: {
			jobId: string;
			request: LibraryManagementDiscardRequest;
			silent?: boolean;
		}) =>
			api.global.post<LibraryManagementPreviewDetailResponse>(
				API.libraryManagement.discardPreview(input.jobId),
				input.request
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
		mutationFn: (input: { jobId: string; request: LibraryManagementUndoPreviewRequest }) =>
			api.global.post<LibraryManagementPreviewCreatedResponse>(
				API.libraryManagement.undoPreview(input.jobId),
				input.request
			),
		onSuccess: showQueued('Undo preview queued'),
		onError: showActionError('Could not queue the undo preview')
	}));

export const createLibraryManagementBaselineRestorePreviewMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementBaselineRestorePreviewRequest) =>
			api.global.post<LibraryManagementPreviewCreatedResponse>(
				API.libraryManagement.baselineRestorePreviews(),
				request
			),
		onSuccess: showQueued('Baseline restore preview queued'),
		onError: showActionError('Could not queue the baseline restore preview')
	}));

export const createLibraryManagementDuplicateResolutionMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementDuplicateResolutionPreviewRequest) =>
			api.global.post<LibraryManagementPreviewCreatedResponse>(
				API.libraryManagement.duplicateResolutionPreviews(),
				request
			),
		onSuccess: showQueued('Duplicate resolution preview queued'),
		onError: showActionError('Could not queue the duplicate resolution preview')
	}));

export const previewLibraryManagementBaselinePurgeMutation = () =>
	createMutation(() => ({
		mutationFn: () =>
			api.global.post<LibraryManagementBaselinePurgeImpactResponse>(
				API.libraryManagement.baselinePurgeImpact()
			)
	}));

export const purgeLibraryManagementBaselinesMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementBaselinePurgeRequest) =>
			api.global.post<LibraryManagementBaselinePurgeResponse>(
				API.libraryManagement.purgeBaselines(),
				request
			),
		onSuccess: showQueued('Organization baselines purged'),
		onError: showActionError('Could not purge organization baselines')
	}));

export const resolveLibraryManagementImportBundleMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { bundleId: string }) =>
			api.global.post<LibraryManagementImportBundleResolveResponse>(
				API.libraryManagement.resolveImportBundle(input.bundleId)
			)
	}));

export const controlLibraryManagementOperationMutation = (action: 'pause' | 'resume' | 'stop') =>
	createMutation(() => ({
		mutationFn: (input: { jobId: string; expectedRevision: number }) => {
			const url =
				action === 'pause'
					? API.library.pauseOperation(input.jobId)
					: action === 'resume'
						? API.library.resumeOperation(input.jobId)
						: API.library.stopOperation(input.jobId);
			return api.global.post<OperationResponse>(url, {
				expected_row_revision: input.expectedRevision
			});
		},
		onSuccess: async () => {
			await invalidateLibraryManagementSurfaces();
			toastStore.show({ message: `Organization ${action} requested`, type: 'success' });
		},
		onError: showActionError(`Could not ${action} this organization operation`)
	}));
