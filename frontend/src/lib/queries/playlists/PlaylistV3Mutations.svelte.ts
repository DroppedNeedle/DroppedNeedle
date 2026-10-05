import { createMutation } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import {
	FavoriteQueryKeyFactory,
	PlaylistQueryKeyFactory,
	type FavoriteV3Kind,
	type PlaylistV3UserId
} from './PlaylistQueryKeyFactory';
import { PlaylistV3Api } from './PlaylistV3Api';

export type CreatePlaylistBodyV3 = components['schemas']['CreatePlaylistBody'];
export type UpdatePlaylistBodyV3 = components['schemas']['UpdatePlaylistBody'];
export type AddTracksBodyV3 = components['schemas']['AddTracksBody'];
export type AddTracksResponseV3 = components['schemas']['AddTracksResponse'];
export type RemoveTracksBodyV3 = components['schemas']['RemoveTracksBody'];
export type UpdateTrackBodyV3 = components['schemas']['UpdateTrackBody'];
export type ReorderBodyV3 = components['schemas']['ReorderBody'];
export type ReorderResponseV3 = components['schemas']['ReorderResponse'];
export type VisibilityBodyV3 = components['schemas']['VisibilityBody'];
export type PlaylistSummaryV3 = components['schemas']['PlaylistSummary'];
export type CoverUploadResponseV3 = components['schemas']['CoverUploadResponse'];
export type CheckTracksBodyV3 = components['schemas']['CheckTracksBody'];
export type CheckTracksResponseV3 = components['schemas']['CheckTracksResponse'];
export type ResolveSourcesResponseV3 = components['schemas']['ResolveSourcesResponse'];
export type TrackInputV3 = components['schemas']['TrackInput'];
export type FavoriteStatusResponseV3 = components['schemas']['FavoriteStatusResponse'];

function invalidateList(userId: PlaylistV3UserId): Promise<unknown> {
	return invalidateQueriesWithPersister({
		queryKey: PlaylistQueryKeyFactory.v3.list(userId)
	});
}

function invalidateDetail(userId: PlaylistV3UserId, id: string): Promise<unknown> {
	return invalidateQueriesWithPersister({
		queryKey: PlaylistQueryKeyFactory.v3.detail(userId, id)
	});
}

function invalidateListAndDetail(userId: PlaylistV3UserId, id: string): Promise<unknown> {
	return Promise.all([invalidateList(userId), invalidateDetail(userId, id)]);
}

export const createPlaylistV3 = () =>
	createMutation(() => ({
		mutationFn: (name: string) => api.global.v3.POST(PlaylistV3Api.create(), { name }),
		onSuccess: () => invalidateList(authStore.user?.id)
	}));

export const updatePlaylistV3 = () =>
	createMutation(() => ({
		mutationFn: (vars: { id: string } & UpdatePlaylistBodyV3) =>
			api.global.v3.PUT(PlaylistV3Api.detail(vars.id), {
				name: vars.name
			}),
		onSuccess: (_data, vars) => invalidateListAndDetail(authStore.user?.id, vars.id)
	}));

export const deletePlaylistV3 = () =>
	createMutation(() => ({
		mutationFn: (id: string) => api.global.v3.DELETE(PlaylistV3Api.detail(id)),
		onSuccess: (_data, id) => invalidateListAndDetail(authStore.user?.id, id)
	}));

export const addPlaylistTracksV3 = () =>
	createMutation(() => ({
		mutationFn: (vars: { id: string } & AddTracksBodyV3) =>
			api.global.v3.POST(PlaylistV3Api.tracks(vars.id), {
				tracks: vars.tracks,
				...(vars.position !== undefined && vars.position !== null
					? { position: vars.position }
					: {})
			}),
		onSuccess: (_data, vars) => invalidateListAndDetail(authStore.user?.id, vars.id)
	}));

export const removePlaylistTrackV3 = () =>
	createMutation(() => ({
		mutationFn: (vars: { id: string; trackId: string }) =>
			api.global.v3.DELETE(PlaylistV3Api.track(vars.id, vars.trackId)),
		onSuccess: (_data, vars) => invalidateListAndDetail(authStore.user?.id, vars.id)
	}));

export const removePlaylistTracksV3 = () =>
	createMutation(() => ({
		mutationFn: (vars: { id: string; trackIds: string[] }) =>
			api.global.v3.POST(PlaylistV3Api.removeTracks(vars.id), {
				track_ids: vars.trackIds
			}),
		onSuccess: (_data, vars) => invalidateListAndDetail(authStore.user?.id, vars.id)
	}));

export const updatePlaylistTrackV3 = () =>
	createMutation(() => ({
		mutationFn: (vars: {
			id: string;
			trackId: string;
			sourceType?: string | null;
			availableSources?: string[] | null;
		}) =>
			api.global.v3.PATCH(PlaylistV3Api.track(vars.id, vars.trackId), {
				...(vars.sourceType !== undefined && vars.sourceType !== null
					? { source_type: vars.sourceType }
					: {}),
				...(vars.availableSources !== undefined && vars.availableSources !== null
					? { available_sources: vars.availableSources }
					: {})
			}),
		// Retargeting a source changes no list tile, so the detail alone refreshes.
		onSuccess: (_data, vars) => invalidateDetail(authStore.user?.id, vars.id)
	}));

export const reorderPlaylistTrackV3 = () =>
	createMutation(() => ({
		mutationFn: (vars: { id: string; trackId: string; newPosition: number }) =>
			api.global.v3.PATCH(PlaylistV3Api.reorderTrack(vars.id), {
				track_id: vars.trackId,
				new_position: vars.newPosition
			}),
		// Order changes no list tile, so the detail alone refreshes.
		onSuccess: (_data, vars) => invalidateDetail(authStore.user?.id, vars.id)
	}));

export const setPlaylistVisibilityV3 = () =>
	createMutation(() => ({
		mutationFn: (vars: { id: string; isPublic: boolean }) =>
			api.global.v3.PATCH(PlaylistV3Api.visibility(vars.id), {
				is_public: vars.isPublic
			}),
		onSuccess: (_data, vars) => invalidateListAndDetail(authStore.user?.id, vars.id)
	}));

async function fileToBase64(file: File): Promise<string> {
	const bytes = new Uint8Array(await file.arrayBuffer());
	let binary = '';
	for (const byte of bytes) binary += String.fromCharCode(byte);
	return btoa(binary);
}

export const uploadPlaylistCoverV3 = () =>
	createMutation(() => ({
		mutationFn: async (vars: { id: string; file: File }) =>
			api.global.v3.POST(PlaylistV3Api.cover(vars.id), {
				content_type: vars.file.type,
				image_base64: await fileToBase64(vars.file)
			}),
		onSuccess: (_data, vars) => invalidateListAndDetail(authStore.user?.id, vars.id)
	}));

export const deletePlaylistCoverV3 = () =>
	createMutation(() => ({
		mutationFn: (id: string) => api.global.v3.DELETE(PlaylistV3Api.cover(id)),
		onSuccess: (_data, id) => invalidateListAndDetail(authStore.user?.id, id)
	}));

export const checkPlaylistTracksV3 = () =>
	createMutation(() => ({
		// Pure read: membership answers are never cached, so no invalidation.
		mutationFn: (body: CheckTracksBodyV3) => api.global.v3.POST(PlaylistV3Api.checkTracks(), body)
	}));

export const resolvePlaylistSourcesV3 = () =>
	createMutation(() => ({
		// Pure read: source answers are never cached, so no invalidation.
		mutationFn: (id: string) => api.global.v3.POST(PlaylistV3Api.resolveSources(id))
	}));

export interface SetFavoriteV3Variables {
	kind: FavoriteV3Kind;
	itemId: string;
	favorited: boolean;
	name?: string | null;
}

export const setFavoriteV3 = () =>
	createMutation(() => ({
		mutationFn: (vars: SetFavoriteV3Variables) =>
			api.global.v3.PUT(PlaylistV3Api.favorite(vars.kind, vars.itemId), {
				favorited: vars.favorited,
				...(vars.name !== undefined ? { name: vars.name } : {})
			}),
		// The flag lives in two places: the favorites ledger and the
		// favorite-bearing library views, so both sweep together.
		onSuccess: async () => {
			const userId = authStore.user?.id;
			await invalidateQueriesWithPersister({
				queryKey: FavoriteQueryKeyFactory.user(userId)
			});
			await invalidateQueriesWithPersister({
				queryKey: LibraryQueryKeyFactory.v3.root(userId)
			});
		}
	}));
