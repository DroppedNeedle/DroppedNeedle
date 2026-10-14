import { v3 } from '$lib/api/v3/endpoint';
import type { LibraryManagementHistoryParams, LibraryManagementPlanItemParams } from './types';

const job = (jobId: string) => ({ path: { job_id: jobId } });

// v3 Library Management URLs: settings, profiles and activation dry runs,
// then previews, operations, the tag editor, originals, duplicates and
// recovery.
export const LIBRARY_MANAGEMENT_ENDPOINTS = {
	settings: () => v3('/api/v3/settings/library-management'),
	activationPreviews: () => v3('/api/v3/settings/library-management/activation-previews'),
	activationPreview: (jobId: string) =>
		v3('/api/v3/settings/library-management/activation-previews/{job_id}', job(jobId)),
	activationConfirmations: () => v3('/api/v3/settings/library-management/activation-confirmations'),
	tagEditor: (trackId: string) =>
		v3('/api/v3/library/management/tracks/{track_id}/tag-editor', {
			path: { track_id: trackId }
		}),
	tagEditPreviews: () => v3('/api/v3/library/management/tag-edit-previews'),
	baselineRestorePreviews: () => v3('/api/v3/library/management/baselines/restore-previews'),
	baselinePurgeImpact: () => v3('/api/v3/library/management/baselines/purge-impact'),
	purgeBaselines: () => v3('/api/v3/library/management/baselines/purge'),
	duplicateResolutionPreviews: () => v3('/api/v3/library/management/duplicate-resolution-previews'),
	recoveryDiagnostics: () => v3('/api/v3/library/management/recovery/diagnostics'),
	automaticSkipped: () => v3('/api/v3/library/management/automatic/skipped'),
	takeovers: () => v3('/api/v3/library/management/takeovers'),
	handBackAll: () => v3('/api/v3/library/management/hand-back'),
	handBackAlbum: (albumId: string) =>
		v3('/api/v3/library/albums/{album_id}/management/hand-back', {
			path: { album_id: albumId }
		}),
	resolveImportBundle: (bundleId: string) =>
		v3('/api/v3/library/management/recovery/import-bundles/{bundle_id}/resolve', {
			path: { bundle_id: bundleId }
		}),
	impact: () => v3('/api/v3/settings/library-management/impact'),
	validate: () => v3('/api/v3/settings/library-management/validate'),
	activationHealth: () => v3('/api/v3/settings/library-management/activation-health'),
	placement: () => v3('/api/v3/settings/library-management/placement'),
	checkPlacement: (rootId: string) =>
		v3('/api/v3/settings/library-management/placement/{root_id}/check', {
			path: { root_id: rootId }
		}),
	profiles: () => v3('/api/v3/settings/library-management/profiles'),
	profile: (profileId: string) =>
		v3('/api/v3/settings/library-management/profiles/{profile_id}', {
			path: { profile_id: profileId }
		}),
	copyProfile: (profileId: string) =>
		v3('/api/v3/settings/library-management/profiles/{profile_id}/copy', {
			path: { profile_id: profileId }
		}),
	exportProfile: (profileId: string) =>
		v3('/api/v3/settings/library-management/profiles/{profile_id}/export', {
			path: { profile_id: profileId }
		}),
	presetDiff: (profileId: string) =>
		v3('/api/v3/settings/library-management/profiles/{profile_id}/preset-diff', {
			path: { profile_id: profileId }
		}),
	profileImportPreview: () => v3('/api/v3/settings/library-management/profile-imports/preview'),
	profileImports: () => v3('/api/v3/settings/library-management/profile-imports'),
	previews: () => v3('/api/v3/library/management/previews'),
	previewArtwork: (jobId: string, ordinal: number, sha256: string) =>
		v3('/api/v3/library/management/previews/{job_id}/items/{ordinal}/artwork/{sha256}', {
			path: { job_id: jobId, ordinal, sha256 }
		}),
	preview: (jobId: string) => v3('/api/v3/library/management/previews/{job_id}', job(jobId)),
	previewItems: (jobId: string, params: LibraryManagementPlanItemParams = {}) =>
		v3('/api/v3/library/management/previews/{job_id}/items', {
			...job(jobId),
			query: {
				after_ordinal: params.afterOrdinal,
				limit: params.limit,
				eligibility: params.eligibility,
				reason_code: params.reasonCode,
				root_id: params.rootId,
				album_id: params.albumId,
				audio_format: params.audioFormat,
				change_kind: params.changeKind
			}
		}),
	discardPreview: (jobId: string) =>
		v3('/api/v3/library/management/previews/{job_id}/discard', job(jobId)),
	applyPreview: (jobId: string) =>
		v3('/api/v3/library/management/previews/{job_id}/apply', job(jobId)),
	reissuePreview: (jobId: string) =>
		v3('/api/v3/library/management/previews/{job_id}/reissue', job(jobId)),
	operations: (params: LibraryManagementHistoryParams = {}) =>
		v3('/api/v3/library/management/operations', {
			query: {
				limit: params.limit,
				cursor: params.cursor,
				origin: params.origin,
				profile_id: params.profileId,
				root_id: params.rootId,
				state: params.state,
				mode: params.mode,
				created_from: params.createdFrom,
				created_to: params.createdTo
			}
		}),
	operation: (jobId: string) => v3('/api/v3/library/management/operations/{job_id}', job(jobId)),
	operationResults: (jobId: string, afterOrdinal?: number, limit?: number) =>
		v3('/api/v3/library/management/operations/{job_id}/results', {
			...job(jobId),
			query: { after_ordinal: afterOrdinal, limit }
		}),
	undoPreview: (jobId: string) =>
		v3('/api/v3/library/management/operations/{job_id}/undo-preview', job(jobId))
} as const;
