import { createMutation } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { toastStore } from '$lib/stores/toast';
import { invalidateLibraryCatalog } from './LibraryCatalogInvalidation';
import { LibraryV3Api } from './LibraryV3Api';
import { toOperationResponse } from './libraryOperationAdapters';
import { createUuid } from '$lib/utils/uuid';
import type {
	CatalogCorrectionResponse,
	MembershipPreviewResponse,
	OperationResponse
} from './LibraryOperationsTypes';

export interface MembershipPreviewInput {
	track_ids: string[];
	expected_album_revisions: Record<string, number>;
	target_album_id?: string | null;
	title?: string | null;
	album_artist_name?: string | null;
	identity_choice?: 'detach' | 'retain_manual';
}

export interface ArtistMergePreviewInput {
	source_artist_ids: string[];
	surviving_artist_id: string;
	expected_revisions: Record<string, number>;
	provider_choice?: 'detach' | 'retain_survivor';
}

// Re-identification evaluates candidates as an operation the user confirms:
// nothing seals until a candidate is chosen. The album views carry no
// revisions yet (0 and ''), so those guards are only sent when known.
export function reidentifyLibraryAlbum() {
	return createMutation(() => ({
		mutationFn: async (input: {
			albumId: string;
			expectedAlbumRevision: number;
			expectedInputRevision: string;
			oneOffLocalMetadata: boolean;
			releaseMbid?: string | null;
		}): Promise<OperationResponse> =>
			toOperationResponse(
				await api.global.v3.POST(LibraryV3Api.reidentifyAlbum(input.albumId), {
					expected_album_revision:
						input.expectedAlbumRevision > 0 ? input.expectedAlbumRevision : null,
					expected_input_revision: input.expectedInputRevision || null,
					idempotency_key: createUuid(),
					one_off_local_metadata: input.oneOffLocalMetadata,
					release_mbid: input.releaseMbid ?? null
				})
			),
		onSuccess: async () => {
			await invalidateLibraryCatalog();
			toastStore.show({ message: 'Identification started', type: 'success' });
		},
		onError: () => toastStore.show({ message: 'Could not start identification', type: 'error' })
	}));
}

export function selectReidentificationCandidate() {
	return createMutation(() => ({
		mutationFn: async (input: {
			jobId: string;
			expectedRevision: number;
			candidateKey?: string;
			confirmation: boolean;
			decisionMode?: 'exact_release' | 'custom_edition' | 'leave_unmanaged';
		}): Promise<OperationResponse> =>
			toOperationResponse(
				await api.global.v3.POST(LibraryV3Api.operationCandidate(input.jobId), {
					expected_row_revision: input.expectedRevision,
					candidate_key: input.candidateKey ?? '',
					confirmation: input.confirmation,
					decision_mode: input.decisionMode ?? 'exact_release'
				})
			),
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
			api.global.v3.POST(LibraryV3Api.reenableManagement(input.albumId), {
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

type MembershipKind = 'split' | 'merge' | 'move' | 'reset';

function previewUrl(kind: MembershipKind, albumId: string) {
	switch (kind) {
		case 'split':
			return LibraryV3Api.splitPreview(albumId);
		case 'merge':
			return LibraryV3Api.mergePreview();
		case 'move':
			return LibraryV3Api.movePreview();
		case 'reset':
			return LibraryV3Api.resetGroupingPreview(albumId);
	}
}

function applyUrl(kind: MembershipKind, albumId: string) {
	switch (kind) {
		case 'split':
			return LibraryV3Api.split(albumId);
		case 'merge':
			return LibraryV3Api.merge();
		case 'move':
			return LibraryV3Api.move();
		case 'reset':
			return LibraryV3Api.resetGrouping(albumId);
	}
}

const errorMessage = (error: unknown, fallback: string) =>
	error instanceof Error && error.message ? error.message : fallback;

// Album organization: the preview runs the change and rolls it back, so it
// shows exactly what applying does; the token it returns applies only that.
export function previewAlbumMembership(kind: MembershipKind) {
	return createMutation(() => ({
		mutationFn: (input: {
			albumId: string;
			request: MembershipPreviewInput;
		}): Promise<MembershipPreviewResponse> =>
			api.global.v3.POST(previewUrl(kind, input.albumId), input.request)
	}));
}

export function applyAlbumMembership(kind: MembershipKind) {
	return createMutation(() => ({
		mutationFn: (input: {
			albumId: string;
			request: MembershipPreviewInput;
			previewToken: string;
		}): Promise<CatalogCorrectionResponse> =>
			api.global.v3.POST(applyUrl(kind, input.albumId), {
				...input.request,
				preview_token: input.previewToken,
				idempotency_key: createUuid()
			}),
		onSuccess: async () => {
			await invalidateLibraryCatalog();
			toastStore.show({ message: 'Album organization updated', type: 'success' });
		},
		onError: (error) =>
			toastStore.show({
				message: errorMessage(error, 'Album organization changed; preview it again'),
				type: 'error'
			})
	}));
}

export function previewArtistMerge() {
	return createMutation(() => ({
		mutationFn: (input: ArtistMergePreviewInput): Promise<MembershipPreviewResponse> =>
			api.global.v3.POST(LibraryV3Api.artistMergePreview(), input),
		onError: (error) =>
			toastStore.show({
				message: errorMessage(error, 'Could not preview this artist merge'),
				type: 'error'
			})
	}));
}

export function applyArtistMerge() {
	return createMutation(() => ({
		mutationFn: (
			input: ArtistMergePreviewInput & {
				preview_token: string;
				provider_choice: 'detach' | 'retain_survivor';
			}
		): Promise<CatalogCorrectionResponse> =>
			api.global.v3.POST(LibraryV3Api.artistMerge(), {
				...input,
				idempotency_key: createUuid()
			}),
		onSuccess: async () => {
			await invalidateLibraryCatalog();
			toastStore.show({ message: 'Artists merged', type: 'success' });
		},
		onError: (error) =>
			toastStore.show({
				message: errorMessage(error, 'The artists changed; preview the merge again'),
				type: 'error'
			})
	}));
}
