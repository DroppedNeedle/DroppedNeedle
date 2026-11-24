import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { useQueryClient } from '@tanstack/svelte-query';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { PlaylistQueryKeyFactory } from './PlaylistQueryKeyFactory';
import {
	anyDownloadChanged,
	anyRequestChanged,
	applyEffects,
	createAppMutation,
	pendingApprovalsChanged,
	playlistChanged,
	playlistListChanged,
	playlistRequestStatesChanged,
	playlistSourcesDropped,
	playlistSyncChanged,
	spotifyPlaylistsChanged,
	userQuotaChanged
} from '$lib/queries/effects';

export type UpdatePlaylistBodyV3 = components['schemas']['UpdatePlaylistBody'];
export type AddTracksBodyV3 = components['schemas']['AddTracksBody'];
export type PlaylistSummaryV3 = components['schemas']['PlaylistSummary'];
export type CheckTracksBodyV3 = components['schemas']['CheckTracksBody'];
export type CheckTracksResponseV3 = components['schemas']['CheckTracksResponse'];
export type TrackInputV3 = components['schemas']['TrackInput'];
export type ImportPlaylistBodyV3 = components['schemas']['ImportPlaylistBody'];
export type ImportPlaylistResponseV3 = components['schemas']['ImportPlaylistResponse'];
export type ImportUnmatchedReasonV3 = components['schemas']['ImportUnmatchedReason'];
export type PlaylistRequestResultV3 = components['schemas']['PlaylistRequestResult'];
const list = () => [playlistListChanged(authStore.user?.id)];
const detail = (id: string) => [playlistChanged(authStore.user?.id, id)];
const listAndDetail = (id: string) => [...list(), ...detail(id)];

/** What a playlist request changes: its songs' states, the requests, the
 * downloads, what waits for approval and the caller's quota. */
const requested = () => {
	const userId = authStore.user?.id;
	return [
		playlistRequestStatesChanged(),
		anyRequestChanged(),
		anyDownloadChanged(),
		pendingApprovalsChanged(userId),
		...(userId ? [userQuotaChanged(userId)] : [])
	];
};

/**
 * Ask for a playlist's missing albums, or with `entryId` one row's album.
 * The caller makes `idempotencyKey` once per press, so a retry keeps it.
 */
export const createRequestPlaylistMissingMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: { playlistId: string; idempotencyKey: string; entryId?: string }) =>
			api.v3.POST(
				v3('/api/v3/playlists/{playlist_id}/request-missing', {
					path: { playlist_id: vars.playlistId }
				}),
				{ idempotency_key: vars.idempotencyKey, entry_id: vars.entryId ?? null }
			),
		effects: requested
	}));

/** Ask for one row's song alone. */
export const createRequestPlaylistSongMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: { playlistId: string; entryId: string }) =>
			api.v3.POST(
				v3('/api/v3/playlists/{playlist_id}/tracks/{track_id}/request-song', {
					path: { playlist_id: vars.playlistId, track_id: vars.entryId }
				})
			),
		effects: requested
	}));

/**
 * Turns a playlist's Spotify sync on or off, for its owner. The answer is
 * the sync as the switch left it.
 */
export function createSetPlaylistSyncMutation() {
	const client = useQueryClient();
	return createAppMutation(
		() => ({
			mutationFn: (vars: { playlistId: string; enabled: boolean; wholeAlbums: boolean }) =>
				api.v3.PUT(
					v3('/api/v3/playlists/{playlist_id}/sync', { path: { playlist_id: vars.playlistId } }),
					{ enabled: vars.enabled, whole_albums: vars.wholeAlbums }
				),
			onSuccess: (sync, vars) =>
				setQueryDataWithPersister(
					PlaylistQueryKeyFactory.sync(authStore.user?.id, vars.playlistId),
					sync,
					undefined,
					client
				),
			effects: () => [
				playlistRequestStatesChanged(),
				spotifyPlaylistsChanged(authStore.user?.id),
				pendingApprovalsChanged(authStore.user?.id)
			]
		}),
		client
	);
}

/**
 * Puts a song the sync moved to the artist's own album back on the album
 * Spotify lists it on, for the playlist's owner.
 */
export const createKeepSpotifyAlbumMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: { playlistId: string; entryId: string }) =>
			api.v3.POST(
				v3('/api/v3/playlists/{playlist_id}/tracks/{track_id}/spotify-album', {
					path: { playlist_id: vars.playlistId, track_id: vars.entryId }
				})
			),
		effects: (_data, vars) => [
			...detail(vars.playlistId),
			playlistSyncChanged(authStore.user?.id, vars.playlistId),
			anyRequestChanged(),
			pendingApprovalsChanged(authStore.user?.id)
		]
	}));

/** Reads one playlist again, after its tracks' sources were healed. */
export const refreshPlaylist = (id: string) => applyEffects(...detail(id));

/** Drops one playlist's kept source answer, so the next visit asks again. */
export const forgetPlaylistSources = (id: string) =>
	applyEffects(playlistSourcesDropped(authStore.user?.id, id));

export const createPlaylistV3Mutation = () =>
	createAppMutation(() => ({
		mutationFn: (name: string) => api.v3.POST(v3('/api/v3/playlists'), { name }),
		effects: list
	}));

export const createImportPlaylistV3Mutation = () =>
	createAppMutation(() => ({
		mutationFn: (body: ImportPlaylistBodyV3) => api.v3.POST(v3('/api/v3/playlists/import'), body),
		effects: list
	}));

export const createUpdatePlaylistV3Mutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: { id: string } & UpdatePlaylistBodyV3) =>
			api.v3.PUT(v3('/api/v3/playlists/{playlist_id}', { path: { playlist_id: vars.id } }), {
				name: vars.name
			}),
		effects: (_data, vars) => listAndDetail(vars.id)
	}));

export const createDeletePlaylistV3Mutation = () =>
	createAppMutation(() => ({
		mutationFn: (id: string) =>
			api.v3.DELETE(v3('/api/v3/playlists/{playlist_id}', { path: { playlist_id: id } })),
		effects: (_data, id) => listAndDetail(id)
	}));

export const createAddPlaylistTracksV3Mutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: { id: string } & AddTracksBodyV3) =>
			api.v3.POST(
				v3('/api/v3/playlists/{playlist_id}/tracks', { path: { playlist_id: vars.id } }),
				{
					tracks: vars.tracks,
					...(vars.position !== undefined && vars.position !== null
						? { position: vars.position }
						: {})
				}
			),
		effects: (_data, vars) => listAndDetail(vars.id)
	}));

export const createRemovePlaylistTrackV3Mutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: { id: string; trackId: string }) =>
			api.v3.DELETE(
				v3('/api/v3/playlists/{playlist_id}/tracks/{track_id}', {
					path: { playlist_id: vars.id, track_id: vars.trackId }
				})
			),
		effects: (_data, vars) => listAndDetail(vars.id)
	}));

export const createRemovePlaylistTracksV3Mutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: { id: string; trackIds: string[] }) =>
			api.v3.POST(
				v3('/api/v3/playlists/{playlist_id}/tracks/remove', { path: { playlist_id: vars.id } }),
				{
					track_ids: vars.trackIds
				}
			),
		effects: (_data, vars) => listAndDetail(vars.id)
	}));

export const createUpdatePlaylistTrackV3Mutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: {
			id: string;
			trackId: string;
			sourceType?: string | null;
			availableSources?: string[] | null;
		}) =>
			api.v3.PATCH(
				v3('/api/v3/playlists/{playlist_id}/tracks/{track_id}', {
					path: { playlist_id: vars.id, track_id: vars.trackId }
				}),
				{
					...(vars.sourceType !== undefined && vars.sourceType !== null
						? { source_type: vars.sourceType }
						: {}),
					...(vars.availableSources !== undefined && vars.availableSources !== null
						? { available_sources: vars.availableSources }
						: {})
				}
			),
		// Retargeting a source changes no list tile, so the detail alone refreshes.
		effects: (_data, vars) => detail(vars.id)
	}));

export const createReorderPlaylistTrackV3Mutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: { id: string; trackId: string; newPosition: number }) =>
			api.v3.PATCH(
				v3('/api/v3/playlists/{playlist_id}/tracks/reorder', { path: { playlist_id: vars.id } }),
				{
					track_id: vars.trackId,
					new_position: vars.newPosition
				}
			),
		// Order changes no list tile, so the detail alone refreshes.
		effects: (_data, vars) => detail(vars.id)
	}));

export const createSetPlaylistVisibilityV3Mutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: { id: string; isPublic: boolean }) =>
			api.v3.PATCH(
				v3('/api/v3/playlists/{playlist_id}/visibility', { path: { playlist_id: vars.id } }),
				{
					is_public: vars.isPublic
				}
			),
		effects: (_data, vars) => listAndDetail(vars.id)
	}));

export const createSetPlaylistMembersV3Mutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: { id: string; userIds: string[]; editorIds: string[] }) =>
			api.v3.PUT(
				v3('/api/v3/playlists/{playlist_id}/members', { path: { playlist_id: vars.id } }),
				{ user_ids: vars.userIds, editor_ids: vars.editorIds }
			),
		effects: (_data, vars) => listAndDetail(vars.id)
	}));

export const createLeavePlaylistV3Mutation = () =>
	createAppMutation(() => ({
		mutationFn: (id: string) =>
			api.v3.DELETE(
				v3('/api/v3/playlists/{playlist_id}/members/me', { path: { playlist_id: id } })
			),
		effects: (_data, id) => listAndDetail(id)
	}));

async function fileToBase64(file: File): Promise<string> {
	const bytes = new Uint8Array(await file.arrayBuffer());
	let binary = '';
	for (const byte of bytes) binary += String.fromCharCode(byte);
	return btoa(binary);
}

export const createUploadPlaylistCoverV3Mutation = () =>
	createAppMutation(() => ({
		mutationFn: async (vars: { id: string; file: File }) =>
			api.v3.POST(v3('/api/v3/playlists/{playlist_id}/cover', { path: { playlist_id: vars.id } }), {
				content_type: vars.file.type,
				image_base64: await fileToBase64(vars.file)
			}),
		effects: (_data, vars) => listAndDetail(vars.id)
	}));

export const createDeletePlaylistCoverV3Mutation = () =>
	createAppMutation(() => ({
		mutationFn: (id: string) =>
			api.v3.DELETE(v3('/api/v3/playlists/{playlist_id}/cover', { path: { playlist_id: id } })),
		effects: (_data, id) => listAndDetail(id)
	}));

export const createCheckPlaylistTracksV3Mutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		// Pure read: membership answers are never cached, so no invalidation.
		mutationFn: (body: CheckTracksBodyV3) => api.v3.POST(v3('/api/v3/playlists/check-tracks'), body)
	}));
