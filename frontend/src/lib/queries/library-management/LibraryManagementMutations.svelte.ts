import { catalogChanged, createAppMutation, libraryPlacementChanged } from '$lib/queries/effects';
import { authStore } from '$lib/stores/authStore.svelte';

import { api } from '$lib/api/client';
import { ApiError } from '$lib/api/errors';
import { LIBRARY_MANAGEMENT_ENDPOINTS } from './endpoints';
import { toPreviewDetail } from './libraryManagementAdapters';
import type { OperationResponse } from '$lib/queries/library/LibraryOperationsTypes';
import { operationControlUrl } from '$lib/queries/library/LibraryOperationMutations.svelte';
import { toOperationResponse } from '$lib/queries/library/libraryOperationAdapters';
import { toastStore } from '$lib/stores/toast';

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

const showQueued = (message: string) => () => {
	toastStore.show({ message, type: 'success' });
};

const catalog = () => [catalogChanged(authStore.user?.id)];

export const updateLibraryManagementSettingsMutation = () =>
	createAppMutation(() => ({
		mutationFn: (request: LibraryManagementSettingsUpdateRequest) =>
			api.v3.PUT(LIBRARY_MANAGEMENT_ENDPOINTS.settings(), request),
		effects: catalog
	}));

export const previewLibraryManagementSettingsImpactMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (request: LibraryManagementSettingsImpactRequest) =>
			api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.impact(), request)
	}));

export const validateLibraryManagementSettingsMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (request: LibraryManagementSettingsImpactRequest) =>
			api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.validate(), request)
	}));

export const copyLibraryManagementProfileMutation = () =>
	createAppMutation(() => ({
		mutationFn: (input: { profileId: string; request: LibraryManagementProfileCopyRequest }) =>
			api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.copyProfile(input.profileId), input.request),
		effects: catalog
	}));

export const deleteLibraryManagementProfileMutation = () =>
	createAppMutation(() => ({
		mutationFn: (input: { profileId: string; request: LibraryManagementProfileDeleteRequest }) =>
			api.v3.DELETE(LIBRARY_MANAGEMENT_ENDPOINTS.profile(input.profileId), input.request),
		effects: catalog
	}));

export const exportLibraryManagementProfileMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (input: { profileId: string; request: LibraryManagementProfileExportRequest }) =>
			api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.exportProfile(input.profileId), input.request)
	}));

export const previewLibraryManagementProfileImportMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (request: LibraryManagementProfileImportPreviewRequest) =>
			api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.profileImportPreview(), request)
	}));

export const importLibraryManagementProfileMutation = () =>
	createAppMutation(() => ({
		mutationFn: (request: LibraryManagementProfileImportRequest) =>
			api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.profileImports(), request),
		effects: catalog
	}));

export const createLibraryManagementActivationPreviewMutation = () =>
	createAppMutation(() => ({
		mutationFn: (request: LibraryManagementActivationPreviewRequest) =>
			api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.activationPreviews(), request),
		onSuccess: (result) => {
			toastStore.show({
				message: result.existing ? 'Active dry run resumed' : 'Activation dry run queued',
				type: 'success'
			});
		},
		effects: catalog,
		onError: showActionError('Could not queue the activation preview')
	}));

export const confirmLibraryManagementActivationMutation = () =>
	createAppMutation(() => ({
		mutationFn: (request: LibraryManagementActivationConfirmRequest) =>
			api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.activationConfirmations(), request),
		effects: catalog,
		onSuccess: showQueued('File organization activated'),
		onError: showActionError('Could not activate file organization')
	}));

export const createLibraryManagementPreviewMutation = () =>
	createAppMutation(() => ({
		mutationFn: (
			request: LibraryManagementPreviewCreateRequest
		): Promise<LibraryManagementPreviewCreatedResponse> =>
			api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.previews(), request),
		effects: catalog,
		onSuccess: showQueued('Organization preview queued'),
		onError: showActionError('Could not queue the management preview')
	}));

export const createLibraryManagementTagEditPreviewMutation = () =>
	createAppMutation(() => ({
		mutationFn: (request: LibraryManagementTagEditPreviewRequest) =>
			api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.tagEditPreviews(), request),
		effects: catalog,
		onSuccess: showQueued('Tag edit preview queued'),
		onError: showActionError('Could not queue the tag edit preview')
	}));

export const applyLibraryManagementPreviewMutation = () =>
	createAppMutation(() => ({
		mutationFn: async (input: {
			jobId: string;
			request: LibraryManagementApplyRequest;
		}): Promise<OperationResponse> =>
			toOperationResponse(
				await api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.applyPreview(input.jobId), {
					...input.request,
					confirmation: input.request.confirmation ?? false
				})
			),
		effects: catalog,
		onSuccess: showQueued('Organization work queued'),
		onError: (error: Error) => {
			// The apply dialog replaces a refused token and tries once more.
			if (error instanceof ApiError && error.code === 'PREVIEW_TOKEN_INVALID') return;
			showActionError('Could not apply this management preview')(error);
		}
	}));

export const reissueLibraryManagementPreviewMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (input: {
			jobId: string;
			silent?: boolean;
		}): Promise<LibraryManagementPreviewReissueResponse> =>
			api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.reissuePreview(input.jobId)),
		onError: (error: Error, input: { jobId: string; silent?: boolean }) => {
			if (!input.silent) showActionError('Could not resume this management preview')(error);
		}
	}));

export const discardLibraryManagementPreviewMutation = () =>
	createAppMutation(() => ({
		mutationFn: async (input: {
			jobId: string;
			request: LibraryManagementDiscardRequest;
			silent?: boolean;
		}): Promise<LibraryManagementPreviewDetailResponse> =>
			toPreviewDetail(
				await api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.discardPreview(input.jobId), input.request)
			),
		onSuccess: (
			_response: LibraryManagementPreviewDetailResponse,
			input: { jobId: string; request: LibraryManagementDiscardRequest; silent?: boolean }
		) => {
			if (!input.silent) showQueued('Organization preview discarded')();
		},
		effects: (
			_response: LibraryManagementPreviewDetailResponse,
			input: { jobId: string; request: LibraryManagementDiscardRequest; silent?: boolean }
		) => (input.silent ? [] : catalog()),
		onError: (
			error: Error,
			input: { jobId: string; request: LibraryManagementDiscardRequest; silent?: boolean }
		) => {
			if (!input.silent) showActionError('Could not discard this management preview')(error);
		}
	}));

export const createLibraryManagementUndoPreviewMutation = () =>
	createAppMutation(() => ({
		mutationFn: (input: {
			jobId: string;
			request: LibraryManagementUndoPreviewRequest;
		}): Promise<LibraryManagementPreviewCreatedResponse> =>
			api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.undoPreview(input.jobId), input.request),
		effects: catalog,
		onSuccess: showQueued('Undo preview queued'),
		onError: showActionError('Could not queue the undo preview')
	}));

export const createLibraryManagementBaselineRestorePreviewMutation = () =>
	createAppMutation(() => ({
		mutationFn: (request: LibraryManagementBaselineRestorePreviewRequest) =>
			api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.baselineRestorePreviews(), request),
		effects: catalog,
		onSuccess: showQueued('Baseline restore preview queued'),
		onError: showActionError('Could not queue the baseline restore preview')
	}));

export const createLibraryManagementDuplicateResolutionMutation = () =>
	createAppMutation(() => ({
		mutationFn: (request: LibraryManagementDuplicateResolutionPreviewRequest) =>
			api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.duplicateResolutionPreviews(), request),
		effects: catalog,
		onSuccess: showQueued('Duplicate resolution preview queued'),
		onError: showActionError('Could not queue the duplicate resolution preview')
	}));

export const previewLibraryManagementBaselinePurgeMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: () => api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.baselinePurgeImpact())
	}));

export const purgeLibraryManagementBaselinesMutation = () =>
	createAppMutation(() => ({
		mutationFn: (request: LibraryManagementBaselinePurgeRequest) =>
			api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.purgeBaselines(), request),
		effects: catalog,
		onSuccess: showQueued('Organization baselines purged'),
		onError: showActionError('Could not purge organization baselines')
	}));

export const resolveLibraryManagementImportBundleMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (input: { bundleId: string }) =>
			api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.resolveImportBundle(input.bundleId))
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
	createAppMutation(() => ({
		mutationFn: (input: { albumId: string }) =>
			api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.handBackAlbum(input.albumId)),
		effects: catalog,
		// Unreadable files are a state the page shows beside the button, not a toast.
		onError: (error: Error) => {
			if (handBackRefusalMessage(error) !== null) return;
			showActionError('Could not hand this album back')(error);
		}
	}));

// Hands back every album the app stepped back from. The server works
// through them in the background.
export const handBackAllLibraryAlbumsMutation = () =>
	createAppMutation(() => ({
		mutationFn: () => api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.handBackAll()),
		effects: catalog,
		onError: showActionError('Could not hand these albums back')
	}));

export const controlLibraryManagementOperationMutation = (action: 'pause' | 'resume' | 'stop') =>
	createAppMutation(() => ({
		mutationFn: async (input: {
			jobId: string;
			expectedRevision: number;
		}): Promise<OperationResponse> =>
			toOperationResponse(
				await api.v3.POST(operationControlUrl(action, input.jobId), {
					expected_row_revision: input.expectedRevision
				})
			),
		onSuccess: () => {
			toastStore.show({ message: `Organization ${action} requested`, type: 'success' });
		},
		effects: catalog,
		onError: showActionError(`Could not ${action} this organization operation`)
	}));

// Checks one library folder now. The server creates and removes a folder
// at the top of it.
export const checkLibraryManagementPlacementMutation = (
	getUserId: () => string | null | undefined
) =>
	createAppMutation(() => ({
		mutationFn: (rootId: string) =>
			api.v3.POST(LIBRARY_MANAGEMENT_ENDPOINTS.checkPlacement(rootId)),
		effects: () => [libraryPlacementChanged(getUserId())],
		onSuccess: (result) => {
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
