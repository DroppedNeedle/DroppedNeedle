import { createMutation } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { toastStore } from '$lib/stores/toast';
import { invalidateLibraryCatalog } from './LibraryCatalogInvalidation';
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

export function reidentifyLibraryAlbum() {
	return createMutation(() => ({
		mutationFn: (input: {
			albumId: string;
			expectedAlbumRevision: number;
			expectedInputRevision: string;
			oneOffLocalMetadata: boolean;
		}) =>
			api.global.post<OperationResponse>(API.library.reidentifyAlbum(input.albumId), {
				expected_album_revision: input.expectedAlbumRevision,
				expected_input_revision: input.expectedInputRevision,
				idempotency_key: createUuid(),
				one_off_local_metadata: input.oneOffLocalMetadata
			}),
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
			candidateKey: string;
			confirmation: boolean;
		}) =>
			api.global.post<OperationResponse>(API.library.operationCandidate(input.jobId), {
				expected_row_revision: input.expectedRevision,
				candidate_key: input.candidateKey,
				confirmation: input.confirmation
			}),
		onSuccess: invalidateLibraryCatalog,
		onError: () =>
			toastStore.show({ message: 'The candidates changed; review them again', type: 'error' })
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
