import { createMutation } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { toastStore } from '$lib/stores/toast';
import { invalidateLibraryCatalog } from './LibraryCatalogInvalidation';
import { LibraryV3Api } from './LibraryV3Api';
import { createUuid } from '$lib/utils/uuid';
import type { MembershipPreviewResponse, OperationResponse } from './LibraryOperationsTypes';

export interface MembershipPreviewInput {
	track_ids: string[];
	expected_album_revisions: Record<string, number>;
	target_album_id?: string | null;
	title?: string | null;
	album_artist_name?: string | null;
}

export interface ArtistMergePreviewInput {
	source_artist_ids: string[];
	surviving_artist_id: string;
	expected_revisions: Record<string, number>;
}

interface CatalogCorrectionResponse {
	kind: string;
	track_ids: string[];
	source_album_ids: string[];
	target_album_id: string | null;
	surviving_artist_id: string | null;
	retired_artist_ids: string[];
	catalog_revision: number;
}

// A plain re-identification is the v3 identify job. Pinning the answer to
// one release has no v3 route yet, so that request still asks the old path
// (see the waiting-on-backend list in eslint.config.js).
export function reidentifyLibraryAlbum() {
	return createMutation(() => ({
		mutationFn: async (input: {
			albumId: string;
			expectedAlbumRevision: number;
			expectedInputRevision: string;
			oneOffLocalMetadata: boolean;
			releaseMbid?: string | null;
		}): Promise<{ id: string }> => {
			if (!input.releaseMbid) {
				const job = await api.global.v3.POST(LibraryV3Api.identify(), {
					album_id: input.albumId,
					kind: 'manual'
				});
				return { id: job.job_id };
			}
			return api.global.post<OperationResponse>(API.library.reidentifyAlbum(input.albumId), {
				expected_album_revision: input.expectedAlbumRevision,
				expected_input_revision: input.expectedInputRevision,
				idempotency_key: createUuid(),
				one_off_local_metadata: input.oneOffLocalMetadata,
				release_mbid: input.releaseMbid
			});
		},
		onSuccess: async () => {
			await invalidateLibraryCatalog();
			toastStore.show({ message: 'Identification started', type: 'success' });
		},
		onError: () => toastStore.show({ message: 'Could not start identification', type: 'error' })
	}));
}

export function selectReidentificationCandidate() {
	return createMutation(() => ({
		mutationFn: (input: {
			jobId: string;
			expectedRevision: number;
			candidateKey?: string;
			confirmation: boolean;
			decisionMode?: 'exact_release' | 'custom_edition' | 'leave_unmanaged';
		}) =>
			api.global.post<OperationResponse>(API.library.operationCandidate(input.jobId), {
				expected_row_revision: input.expectedRevision,
				candidate_key: input.candidateKey ?? '',
				confirmation: input.confirmation,
				decision_mode: input.decisionMode ?? 'exact_release'
			}),
		onSuccess: invalidateLibraryCatalog,
		onError: (error) =>
			toastStore.show({
				message: error instanceof Error ? error.message : 'Could not save this album decision',
				type: 'error'
			})
	}));
}

export function reenableAlbumManagement() {
	return createMutation(() => ({
		mutationFn: (input: { albumId: string; expectedRevision: number }) =>
			api.global.post<{ reenabled: boolean }>(API.library.reenableAlbumManagement(input.albumId), {
				expected_exclusion_revision: input.expectedRevision
			}),
		onSuccess: async () => {
			await invalidateLibraryCatalog();
			toastStore.show({ message: 'File organization re-enabled', type: 'success' });
		},
		onError: (error) =>
			toastStore.show({
				message: error instanceof Error ? error.message : 'Could not re-enable file organization',
				type: 'error'
			})
	}));
}

export function previewAlbumMembership(kind: 'split' | 'merge' | 'move' | 'reset') {
	return createMutation(() => ({
		mutationFn: (input: { albumId: string; request: MembershipPreviewInput }) => {
			const url =
				kind === 'split'
					? API.library.previewAlbumSplit(input.albumId)
					: kind === 'merge'
						? API.library.previewAlbumMerge()
						: kind === 'move'
							? API.library.previewTrackMove()
							: API.library.previewResetAlbumGrouping(input.albumId);
			return api.global.post<MembershipPreviewResponse>(url, input.request);
		}
	}));
}

export function applyAlbumMembership(kind: 'split' | 'merge' | 'move' | 'reset') {
	return createMutation(() => ({
		mutationFn: (input: {
			albumId: string;
			request: MembershipPreviewInput;
			previewToken: string;
			identityChoice: 'detach' | 'retain_manual';
		}) => {
			const url =
				kind === 'split'
					? API.library.splitAlbum(input.albumId)
					: kind === 'merge'
						? API.library.mergeAlbums()
						: kind === 'move'
							? API.library.moveTracks()
							: API.library.resetAlbumGrouping(input.albumId);
			return api.global.post<CatalogCorrectionResponse>(url, {
				...input.request,
				preview_token: input.previewToken,
				idempotency_key: createUuid(),
				identity_choice: input.identityChoice
			});
		},
		onSuccess: async () => {
			await invalidateLibraryCatalog();
			toastStore.show({ message: 'Album organization updated', type: 'success' });
		},
		onError: () =>
			toastStore.show({ message: 'Album organization changed; preview it again', type: 'error' })
	}));
}

export function previewArtistMerge() {
	return createMutation(() => ({
		mutationFn: (input: ArtistMergePreviewInput) =>
			api.global.post<MembershipPreviewResponse>(API.library.previewArtistMerge(), input)
	}));
}

export function applyArtistMerge() {
	return createMutation(() => ({
		mutationFn: (
			input: ArtistMergePreviewInput & {
				preview_token: string;
				provider_choice: 'detach' | 'retain_survivor';
			}
		) =>
			api.global.post<CatalogCorrectionResponse>(API.library.mergeArtists(), {
				...input,
				idempotency_key: createUuid()
			}),
		onSuccess: async () => {
			await invalidateLibraryCatalog();
			toastStore.show({ message: 'Artists merged', type: 'success' });
		},
		onError: () =>
			toastStore.show({ message: 'The artists changed; preview the merge again', type: 'error' })
	}));
}
