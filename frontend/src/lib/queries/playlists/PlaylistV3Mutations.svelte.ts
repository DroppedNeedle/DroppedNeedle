import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { PlaylistV3Api } from './PlaylistV3Api';
import {
	applyEffects,
	createAppMutation,
	playlistChanged,
	playlistListChanged,
	playlistSourcesDropped
} from '$lib/queries/effects';

export type UpdatePlaylistBodyV3 = components['schemas']['UpdatePlaylistBody'];
export type AddTracksBodyV3 = components['schemas']['AddTracksBody'];
export type PlaylistSummaryV3 = components['schemas']['PlaylistSummary'];
export type CheckTracksBodyV3 = components['schemas']['CheckTracksBody'];
export type CheckTracksResponseV3 = components['schemas']['CheckTracksResponse'];
export type TrackInputV3 = components['schemas']['TrackInput'];
const list = () => [playlistListChanged(authStore.user?.id)];
const detail = (id: string) => [playlistChanged(authStore.user?.id, id)];
const listAndDetail = (id: string) => [...list(), ...detail(id)];

/** Reads one playlist again, after its tracks' sources were healed. */
export const refreshPlaylist = (id: string) => applyEffects(...detail(id));

/** Drops one playlist's kept source answer, so the next visit asks again. */
export const forgetPlaylistSources = (id: string) =>
	applyEffects(playlistSourcesDropped(authStore.user?.id, id));

export const createPlaylistV3 = () =>
	createAppMutation(() => ({
		mutationFn: (name: string) => api.v3.POST(PlaylistV3Api.create(), { name }),
		effects: list
	}));

export const updatePlaylistV3 = () =>
	createAppMutation(() => ({
		mutationFn: (vars: { id: string } & UpdatePlaylistBodyV3) =>
			api.v3.PUT(PlaylistV3Api.detail(vars.id), {
				name: vars.name
			}),
		effects: (_data, vars) => listAndDetail(vars.id)
	}));

export const deletePlaylistV3 = () =>
	createAppMutation(() => ({
		mutationFn: (id: string) => api.v3.DELETE(PlaylistV3Api.detail(id)),
		effects: (_data, id) => listAndDetail(id)
	}));

export const addPlaylistTracksV3 = () =>
	createAppMutation(() => ({
		mutationFn: (vars: { id: string } & AddTracksBodyV3) =>
			api.v3.POST(PlaylistV3Api.tracks(vars.id), {
				tracks: vars.tracks,
				...(vars.position !== undefined && vars.position !== null
					? { position: vars.position }
					: {})
			}),
		effects: (_data, vars) => listAndDetail(vars.id)
	}));

export const removePlaylistTrackV3 = () =>
	createAppMutation(() => ({
		mutationFn: (vars: { id: string; trackId: string }) =>
			api.v3.DELETE(PlaylistV3Api.track(vars.id, vars.trackId)),
		effects: (_data, vars) => listAndDetail(vars.id)
	}));

export const removePlaylistTracksV3 = () =>
	createAppMutation(() => ({
		mutationFn: (vars: { id: string; trackIds: string[] }) =>
			api.v3.POST(PlaylistV3Api.removeTracks(vars.id), {
				track_ids: vars.trackIds
			}),
		effects: (_data, vars) => listAndDetail(vars.id)
	}));

export const updatePlaylistTrackV3 = () =>
	createAppMutation(() => ({
		mutationFn: (vars: {
			id: string;
			trackId: string;
			sourceType?: string | null;
			availableSources?: string[] | null;
		}) =>
			api.v3.PATCH(PlaylistV3Api.track(vars.id, vars.trackId), {
				...(vars.sourceType !== undefined && vars.sourceType !== null
					? { source_type: vars.sourceType }
					: {}),
				...(vars.availableSources !== undefined && vars.availableSources !== null
					? { available_sources: vars.availableSources }
					: {})
			}),
		// Retargeting a source changes no list tile, so the detail alone refreshes.
		effects: (_data, vars) => detail(vars.id)
	}));

export const reorderPlaylistTrackV3 = () =>
	createAppMutation(() => ({
		mutationFn: (vars: { id: string; trackId: string; newPosition: number }) =>
			api.v3.PATCH(PlaylistV3Api.reorderTrack(vars.id), {
				track_id: vars.trackId,
				new_position: vars.newPosition
			}),
		// Order changes no list tile, so the detail alone refreshes.
		effects: (_data, vars) => detail(vars.id)
	}));

export const setPlaylistVisibilityV3 = () =>
	createAppMutation(() => ({
		mutationFn: (vars: { id: string; isPublic: boolean }) =>
			api.v3.PATCH(PlaylistV3Api.visibility(vars.id), {
				is_public: vars.isPublic
			}),
		effects: (_data, vars) => listAndDetail(vars.id)
	}));

async function fileToBase64(file: File): Promise<string> {
	const bytes = new Uint8Array(await file.arrayBuffer());
	let binary = '';
	for (const byte of bytes) binary += String.fromCharCode(byte);
	return btoa(binary);
}

export const uploadPlaylistCoverV3 = () =>
	createAppMutation(() => ({
		mutationFn: async (vars: { id: string; file: File }) =>
			api.v3.POST(PlaylistV3Api.cover(vars.id), {
				content_type: vars.file.type,
				image_base64: await fileToBase64(vars.file)
			}),
		effects: (_data, vars) => listAndDetail(vars.id)
	}));

export const deletePlaylistCoverV3 = () =>
	createAppMutation(() => ({
		mutationFn: (id: string) => api.v3.DELETE(PlaylistV3Api.cover(id)),
		effects: (_data, id) => listAndDetail(id)
	}));

export const checkPlaylistTracksV3 = () =>
	createAppMutation(() => ({
		effects: () => [],
		// Pure read: membership answers are never cached, so no invalidation.
		mutationFn: (body: CheckTracksBodyV3) => api.v3.POST(PlaylistV3Api.checkTracks(), body)
	}));
