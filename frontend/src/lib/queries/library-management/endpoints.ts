import { v3 } from '$lib/api/v3/endpoint';
import type { LibraryManagementHistoryParams, LibraryManagementPlanItemParams } from './types';

const job = (jobId: string) => ({ path: { job_id: jobId } });

// v3 Library Management URLs: settings and profiles, then previews and
// operations. Recovery, the tag editor, baselines, duplicate resolution,
// activation dry runs and preview artwork have no v3 route yet and stay in
// the waiting-on-backend list (eslint.config.js).
export const LIBRARY_MANAGEMENT_ENDPOINTS = {
	settings: () => v3('/api/v3/settings/library-management'),
	impact: () => v3('/api/v3/settings/library-management/impact'),
	validate: () => v3('/api/v3/settings/library-management/validate'),
	activationHealth: () => v3('/api/v3/settings/library-management/activation-health'),
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
