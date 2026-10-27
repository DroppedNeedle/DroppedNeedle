import { createMutation } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { toastStore } from '$lib/stores/toast';
import { invalidateLibraryCatalog } from './LibraryCatalogInvalidation';
import { invalidateLibraryManagementSurfaces } from '$lib/queries/library-management/LibraryManagementInvalidation';
import { LibraryV3Api } from './LibraryV3Api';
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

export function reenableAlbumManagement() {
	return createMutation(() => ({
		mutationFn: (input: { albumId: string; expectedRevision: number }) =>
			api.global.v3.POST(LibraryV3Api.reenableManagement(input.albumId), {
				expected_exclusion_revision: input.expectedRevision
			}),
		onSuccess: async () => {
			await Promise.all([invalidateLibraryCatalog(), invalidateLibraryManagementSurfaces()]);
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
		mutationFn: (input: ArtistMergePreviewInput): Promise<ArtistMergePreviewResponse> =>
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
