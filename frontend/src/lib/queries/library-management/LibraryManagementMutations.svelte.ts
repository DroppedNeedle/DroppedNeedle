import { v3 } from '$lib/api/v3/endpoint';
import { catalogChanged, createAppMutation, libraryPlacementChanged } from '$lib/queries/effects';
import { authStore } from '$lib/stores/authStore.svelte';

import { api } from '$lib/api/client';
import { ApiError } from '$lib/api/errors';
import { toPreviewDetail } from './libraryManagementAdapters';
import type { OperationResponse } from '$lib/queries/library/LibraryOperationsTypes';
import { operationControlUrl } from '$lib/queries/library/LibraryOperationMutations.svelte';
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

export const createUpdateLibraryManagementSettingsMutation = () =>
	createAppMutation(() => ({
		mutationFn: (request: LibraryManagementSettingsUpdateRequest) =>
			api.v3.PUT(v3('/api/v3/settings/library-management'), request),
		effects: catalog
	}));

export const createPreviewLibraryManagementSettingsImpactMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (request: LibraryManagementSettingsImpactRequest) =>
			api.v3.POST(v3('/api/v3/settings/library-management/impact'), request)
	}));

export const createValidateLibraryManagementSettingsMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (request: LibraryManagementSettingsImpactRequest) =>
			api.v3.POST(v3('/api/v3/settings/library-management/validate'), request)
	}));

export const createCopyLibraryManagementProfileMutation = () =>
	createAppMutation(() => ({
		mutationFn: (input: { profileId: string; request: LibraryManagementProfileCopyRequest }) =>
			api.v3.POST(
				v3('/api/v3/settings/library-management/profiles/{profile_id}/copy', {
					path: { profile_id: input.profileId }
				}),
				input.request
			),
		effects: catalog
	}));

export const createDeleteLibraryManagementProfileMutation = () =>
	createAppMutation(() => ({
		mutationFn: (input: { profileId: string; request: LibraryManagementProfileDeleteRequest }) =>
			api.v3.DELETE(
				v3('/api/v3/settings/library-management/profiles/{profile_id}', {
					path: { profile_id: input.profileId }
				}),
				input.request
			),
		effects: catalog
	}));

export const createExportLibraryManagementProfileMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (input: { profileId: string; request: LibraryManagementProfileExportRequest }) =>
			api.v3.POST(
				v3('/api/v3/settings/library-management/profiles/{profile_id}/export', {
					path: { profile_id: input.profileId }
				}),
				input.request
			)
	}));

export const createPreviewLibraryManagementProfileImportMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (request: LibraryManagementProfileImportPreviewRequest) =>
			api.v3.POST(v3('/api/v3/settings/library-management/profile-imports/preview'), request)
	}));

export const createImportLibraryManagementProfileMutation = () =>
	createAppMutation(() => ({
		mutationFn: (request: LibraryManagementProfileImportRequest) =>
			api.v3.POST(v3('/api/v3/settings/library-management/profile-imports'), request),
		effects: catalog
	}));

export const createLibraryManagementActivationPreviewMutation = () =>
	createAppMutation(() => ({
		mutationFn: (request: LibraryManagementActivationPreviewRequest) =>
			api.v3.POST(v3('/api/v3/settings/library-management/activation-previews'), request),
		onSuccess: (result) => {
			toastStore.show({
				message: result.existing ? 'Active dry run resumed' : 'Activation dry run queued',
				type: 'success'
			});
		},
		effects: catalog,
		onError: showActionError('Could not queue the activation preview')
	}));

export const createConfirmLibraryManagementActivationMutation = () =>
	createAppMutation(() => ({
		mutationFn: (request: LibraryManagementActivationConfirmRequest) =>
			api.v3.POST(v3('/api/v3/settings/library-management/activation-confirmations'), request),
		effects: catalog,
		onSuccess: showQueued('File organization activated'),
		onError: showActionError('Could not activate file organization')
	}));

export const createLibraryManagementPreviewMutation = () =>
	createAppMutation(() => ({
		mutationFn: (
			request: LibraryManagementPreviewCreateRequest
		): Promise<LibraryManagementPreviewCreatedResponse> =>
			api.v3.POST(v3('/api/v3/library/management/previews'), request),
		effects: catalog,
		onSuccess: showQueued('Organization preview queued'),
		onError: showActionError('Could not queue the management preview')
	}));

export const createLibraryManagementTagEditPreviewMutation = () =>
	createAppMutation(() => ({
		mutationFn: (request: LibraryManagementTagEditPreviewRequest) =>
			api.v3.POST(v3('/api/v3/library/management/tag-edit-previews'), request),
		effects: catalog,
		onSuccess: showQueued('Tag edit preview queued'),
		onError: showActionError('Could not queue the tag edit preview')
	}));

export const createApplyLibraryManagementPreviewMutation = () =>
	createAppMutation(() => ({
		mutationFn: async (input: {
			jobId: string;
			request: LibraryManagementApplyRequest;
		}): Promise<OperationResponse> =>
			await api.v3.POST(
				v3('/api/v3/library/management/previews/{job_id}/apply', { path: { job_id: input.jobId } }),
				{
					...input.request,
					confirmation: input.request.confirmation ?? false
				}
			),
		effects: catalog,
		onSuccess: showQueued('Organization work queued'),
		onError: (error: Error) => {
			// The apply dialog replaces a refused token and tries once more.
			if (error instanceof ApiError && error.code === 'PREVIEW_TOKEN_INVALID') return;
			showActionError('Could not apply this management preview')(error);
		}
	}));

export const createReissueLibraryManagementPreviewMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (input: {
			jobId: string;
			silent?: boolean;
		}): Promise<LibraryManagementPreviewReissueResponse> =>
			api.v3.POST(
				v3('/api/v3/library/management/previews/{job_id}/reissue', {
					path: { job_id: input.jobId }
				})
			),
		onError: (error: Error, input: { jobId: string; silent?: boolean }) => {
			if (!input.silent) showActionError('Could not resume this management preview')(error);
		}
	}));

export const createDiscardLibraryManagementPreviewMutation = () =>
	createAppMutation(() => ({
		mutationFn: async (input: {
			jobId: string;
			request: LibraryManagementDiscardRequest;
			silent?: boolean;
		}): Promise<LibraryManagementPreviewDetailResponse> =>
			toPreviewDetail(
				await api.v3.POST(
					v3('/api/v3/library/management/previews/{job_id}/discard', {
						path: { job_id: input.jobId }
					}),
					input.request
				)
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
			api.v3.POST(
				v3('/api/v3/library/management/operations/{job_id}/undo-preview', {
					path: { job_id: input.jobId }
				}),
				input.request
			),
		effects: catalog,
		onSuccess: showQueued('Undo preview queued'),
		onError: showActionError('Could not queue the undo preview')
	}));

export const createLibraryManagementBaselineRestorePreviewMutation = () =>
	createAppMutation(() => ({
		mutationFn: (request: LibraryManagementBaselineRestorePreviewRequest) =>
			api.v3.POST(v3('/api/v3/library/management/baselines/restore-previews'), request),
		effects: catalog,
		onSuccess: showQueued('Baseline restore preview queued'),
		onError: showActionError('Could not queue the baseline restore preview')
	}));

export const createLibraryManagementDuplicateResolutionMutation = () =>
	createAppMutation(() => ({
		mutationFn: (request: LibraryManagementDuplicateResolutionPreviewRequest) =>
			api.v3.POST(v3('/api/v3/library/management/duplicate-resolution-previews'), request),
		effects: catalog,
		onSuccess: showQueued('Duplicate resolution preview queued'),
		onError: showActionError('Could not queue the duplicate resolution preview')
	}));

export const createPreviewLibraryManagementBaselinePurgeMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: () => api.v3.POST(v3('/api/v3/library/management/baselines/purge-impact'))
	}));

export const createPurgeLibraryManagementBaselinesMutation = () =>
	createAppMutation(() => ({
		mutationFn: (request: LibraryManagementBaselinePurgeRequest) =>
			api.v3.POST(v3('/api/v3/library/management/baselines/purge'), request),
		effects: catalog,
		onSuccess: showQueued('Organization baselines purged'),
		onError: showActionError('Could not purge organization baselines')
	}));

export const createResolveLibraryManagementImportBundleMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (input: { bundleId: string }) =>
			api.v3.POST(
				v3('/api/v3/library/management/recovery/import-bundles/{bundle_id}/resolve', {
					path: { bundle_id: input.bundleId }
				})
			)
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
export const createHandBackLibraryAlbumMutation = () =>
	createAppMutation(() => ({
		mutationFn: (input: { albumId: string }) =>
			api.v3.POST(
				v3('/api/v3/library/albums/{album_id}/management/hand-back', {
					path: { album_id: input.albumId }
				})
			),
		effects: catalog,
		// Unreadable files are a state the page shows beside the button, not a toast.
		onError: (error: Error) => {
			if (handBackRefusalMessage(error) !== null) return;
			showActionError('Could not hand this album back')(error);
		}
	}));

// Hands back every album the app stepped back from. The server works
// through them in the background.
export const createHandBackAllLibraryAlbumsMutation = () =>
	createAppMutation(() => ({
		mutationFn: () => api.v3.POST(v3('/api/v3/library/management/hand-back')),
		effects: catalog,
		onError: showActionError('Could not hand these albums back')
	}));

export const createControlLibraryManagementOperationMutation = (
	action: 'pause' | 'resume' | 'stop'
) =>
	createAppMutation(() => ({
		mutationFn: async (input: {
			jobId: string;
			expectedRevision: number;
		}): Promise<OperationResponse> =>
			await api.v3.POST(operationControlUrl(action, input.jobId), {
				expected_row_revision: input.expectedRevision
			}),
		onSuccess: () => {
			toastStore.show({ message: `Organization ${action} requested`, type: 'success' });
		},
		effects: catalog,
		onError: showActionError(`Could not ${action} this organization operation`)
	}));

// Checks one library folder now. The server creates and removes a folder
// at the top of it.
export const createCheckLibraryManagementPlacementMutation = (
	getUserId: () => string | null | undefined
) =>
	createAppMutation(() => ({
		mutationFn: (rootId: string) =>
			api.v3.POST(
				v3('/api/v3/settings/library-management/placement/{root_id}/check', {
					path: { root_id: rootId }
				})
			),
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
