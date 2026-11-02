import { v3 } from '$lib/api/v3/endpoint';
import { catalogChanged, createAppMutation } from '$lib/queries/effects';
import { authStore } from '$lib/stores/authStore.svelte';
import { api } from '$lib/api/client';
import { toastStore } from '$lib/stores/toast';
import { createUuid } from '$lib/utils/uuid';
import type {
	ArtistMergePreviewResponse,
	CatalogCorrectionResponse,
	MembershipPreviewResponse
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

export function createReenableAlbumManagementMutation() {
	return createAppMutation(() => ({
		mutationFn: (input: { albumId: string; expectedRevision: number }) =>
			api.v3.POST(
				v3('/api/v3/library/albums/{album_id}/management/re-enable', {
					path: { album_id: input.albumId }
				}),
				{
					expected_exclusion_revision: input.expectedRevision
				}
			),
		onSuccess: () => {
			toastStore.show({ message: 'File organization re-enabled', type: 'success' });
		},
		effects: () => [catalogChanged(authStore.user?.id)],
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
			return v3('/api/v3/library/albums/{album_id}/split-preview', { path: { album_id: albumId } });
		case 'merge':
			return v3('/api/v3/library/albums/merge-preview');
		case 'move':
			return v3('/api/v3/library/tracks/move-preview');
		case 'reset':
			return v3('/api/v3/library/albums/{album_id}/reset-grouping-preview', {
				path: { album_id: albumId }
			});
	}
}

function applyUrl(kind: MembershipKind, albumId: string) {
	switch (kind) {
		case 'split':
			return v3('/api/v3/library/albums/{album_id}/split', { path: { album_id: albumId } });
		case 'merge':
			return v3('/api/v3/library/albums/merge');
		case 'move':
			return v3('/api/v3/library/tracks/move');
		case 'reset':
			return v3('/api/v3/library/albums/{album_id}/reset-grouping', {
				path: { album_id: albumId }
			});
	}
}

const errorMessage = (error: unknown, fallback: string) =>
	error instanceof Error && error.message ? error.message : fallback;

// Album organization: the preview runs the change and rolls it back, so it
// shows exactly what applying does; the token it returns applies only that.
export function createPreviewAlbumMembershipMutation(kind: MembershipKind) {
	return createAppMutation(() => ({
		effects: () => [],
		mutationFn: (input: {
			albumId: string;
			request: MembershipPreviewInput;
		}): Promise<MembershipPreviewResponse> =>
			api.v3.POST(previewUrl(kind, input.albumId), input.request)
	}));
}

export function createApplyAlbumMembershipMutation(kind: MembershipKind) {
	return createAppMutation(() => ({
		mutationFn: (input: {
			albumId: string;
			request: MembershipPreviewInput;
			previewToken: string;
		}): Promise<CatalogCorrectionResponse> =>
			api.v3.POST(applyUrl(kind, input.albumId), {
				...input.request,
				preview_token: input.previewToken,
				idempotency_key: createUuid()
			}),
		onSuccess: () => {
			toastStore.show({ message: 'Album organization updated', type: 'success' });
		},
		effects: () => [catalogChanged(authStore.user?.id)],
		onError: (error) =>
			toastStore.show({
				message: errorMessage(error, 'Album organization changed; preview it again'),
				type: 'error'
			})
	}));
}

export function createPreviewArtistMergeMutation() {
	return createAppMutation(() => ({
		effects: () => [],
		mutationFn: (input: ArtistMergePreviewInput): Promise<ArtistMergePreviewResponse> =>
			api.v3.POST(v3('/api/v3/library/artists/merge-preview'), input),
		onError: (error) =>
			toastStore.show({
				message: errorMessage(error, 'Could not preview this artist merge'),
				type: 'error'
			})
	}));
}

export function createApplyArtistMergeMutation() {
	return createAppMutation(() => ({
		mutationFn: (
			input: ArtistMergePreviewInput & {
				preview_token: string;
				provider_choice: 'detach' | 'retain_survivor';
			}
		): Promise<CatalogCorrectionResponse> =>
			api.v3.POST(v3('/api/v3/library/artists/merge'), {
				...input,
				idempotency_key: createUuid()
			}),
		onSuccess: () => {
			toastStore.show({ message: 'Artists merged', type: 'success' });
		},
		effects: () => [catalogChanged(authStore.user?.id)],
		onError: (error) =>
			toastStore.show({
				message: errorMessage(error, 'The artists changed; preview the merge again'),
				type: 'error'
			})
	}));
}
