import { createMutation } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { API } from '$lib/constants';
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
	LibraryManagementChangeImpact,
	LibraryManagementDuplicateResolutionPreviewRequest,
	LibraryManagementDiscardRequest,
	LibraryManagementPreviewCreateRequest,
	LibraryManagementPreviewCreatedResponse,
	LibraryManagementPreviewDetailResponse,
	LibraryManagementProfileCopyRequest,
	LibraryManagementProfileCreateRequest,
	LibraryManagementProfileDeleteRequest,
	LibraryManagementProfileMutationResponse,
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
			api.global.put<LibraryManagementSettingsResponse>(API.libraryManagement.settings(), request),
		onSuccess: invalidateLibraryManagementSurfaces
	}));

export const previewLibraryManagementSettingsImpactMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementSettingsImpactRequest) =>
			api.global.post<LibraryManagementChangeImpact>(API.libraryManagement.impact(), request)
	}));

export const validateLibraryManagementSettingsMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementSettingsImpactRequest) =>
			api.global.post<LibraryManagementChangeImpact>(API.libraryManagement.validate(), request)
	}));

export const createLibraryManagementProfileMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementProfileCreateRequest) =>
			api.global.post<LibraryManagementProfileMutationResponse>(
				API.libraryManagement.profiles(),
				request
			),
		onSuccess: invalidateLibraryManagementSurfaces
	}));

export const copyLibraryManagementProfileMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { profileId: string; request: LibraryManagementProfileCopyRequest }) =>
			api.global.post<LibraryManagementProfileMutationResponse>(
				API.libraryManagement.copyProfile(input.profileId),
				input.request
			),
		onSuccess: invalidateLibraryManagementSurfaces
	}));

export const updateLibraryManagementProfileMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { profileId: string; request: LibraryManagementProfileUpdateRequest }) =>
			api.global.put<LibraryManagementProfileMutationResponse>(
				API.libraryManagement.profile(input.profileId),
				input.request
			),
		onSuccess: invalidateLibraryManagementSurfaces
	}));

export const deleteLibraryManagementProfileMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { profileId: string; request: LibraryManagementProfileDeleteRequest }) =>
			api.global.delete<LibraryManagementSettingsResponse>(
				API.libraryManagement.profile(input.profileId),
				{ body: input.request }
			),
		onSuccess: invalidateLibraryManagementSurfaces
	}));

export const createLibraryManagementActivationPreviewMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementActivationPreviewRequest) =>
			api.global.post<LibraryManagementPreviewCreatedResponse>(
				API.libraryManagement.activationPreviews(),
				request
			),
		onSuccess: showQueued('Activation preview queued'),
		onError: showActionError('Could not queue the activation preview')
	}));

export const confirmLibraryManagementActivationMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementActivationConfirmRequest) =>
			api.global.post<LibraryManagementSettingsResponse>(
				API.libraryManagement.activationConfirmations(),
				request
			),
		onSuccess: showQueued('Library Management activation saved'),
		onError: showActionError('Could not activate Library Management')
	}));

export const createLibraryManagementPreviewMutation = () =>
	createMutation(() => ({
		mutationFn: (request: LibraryManagementPreviewCreateRequest) =>
			api.global.post<LibraryManagementPreviewCreatedResponse>(
				API.libraryManagement.previews(),
				request
			),
		onSuccess: showQueued('Library Management preview queued'),
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
		onSuccess: showQueued('Library Management work queued'),
		onError: showActionError('Could not apply this management preview')
	}));

export const discardLibraryManagementPreviewMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { jobId: string; request: LibraryManagementDiscardRequest }) =>
			api.global.post<LibraryManagementPreviewDetailResponse>(
				API.libraryManagement.discardPreview(input.jobId),
				input.request
			),
		onSuccess: showQueued('Library Management preview discarded'),
		onError: showActionError('Could not discard this management preview')
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
		onSuccess: showQueued('Library Management baselines purged'),
		onError: showActionError('Could not purge Library Management baselines')
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
			toastStore.show({ message: `Library Management ${action} requested`, type: 'success' });
		},
		onError: showActionError(`Could not ${action} this Library Management operation`)
	}));
