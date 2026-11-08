import type { LibraryManagementHistoryParams, LibraryManagementPlanItemParams } from './types';

export function previewItemsQuery(params: LibraryManagementPlanItemParams) {
	return {
		cursor: params.cursor,
		limit: params.limit,
		eligibility: params.eligibility,
		reason_code: params.reasonCode,
		root_id: params.rootId,
		artist_id: params.artistId,
		album_id: params.albumId,
		audio_format: params.audioFormat,
		change_kind: params.changeKind,
		scope: params.scope
	};
}

export function operationsQuery(params: LibraryManagementHistoryParams) {
	return {
		limit: params.limit,
		cursor: params.cursor,
		origin: params.origin,
		profile_id: params.profileId,
		root_id: params.rootId,
		state: params.state,
		mode: params.mode,
		created_from: params.createdFrom,
		created_to: params.createdTo
	};
}
