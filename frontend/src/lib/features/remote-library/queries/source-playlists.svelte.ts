import { SourcePlaylistQueryKeyFactory } from '$lib/queries/featureKeys';
import type {
	SourcePlaylistCollection,
	SourcePlaylistDetail,
	SourcePlaylistSource,
	SourcePlaylistSummary
} from '$lib/types';

import { v3 } from '$lib/api/v3/endpoint';
import { createQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import {
	anyLibraryChanged,
	createAppMutation,
	homeChanged,
	playlistsChanged,
	remoteSourceChanged,
	sourcePlaylistsChanged
} from '$lib/queries/effects';

type Getter<T> = () => T;
type PlaylistSummaryView = components['schemas']['RemotesPlaylistSummary'];

function toSummary(playlist: PlaylistSummaryView): SourcePlaylistSummary {
	return {
		id: playlist.id,
		name: playlist.name,
		track_count: playlist.track_count,
		duration_seconds: playlist.duration_secs,
		cover_url: playlist.image_url ?? '',
		is_smart: playlist.is_smart,
		is_imported: playlist.is_imported
	};
}

// The playlist list and the account it belongs to come from two v3 reads.
async function fetchCollection(
	source: SourcePlaylistSource,
	signal: AbortSignal
): Promise<SourcePlaylistCollection> {
	const [playlists, connection] = await Promise.all([
		api.v3.GET(v3('/api/v3/remotes/{source}/playlists', { path: { source } }), { signal }),
		api.v3.GET(v3('/api/v3/remotes/{source}/connection', { path: { source } }), { signal })
	]);
	return {
		account_mode: connection.account_mode === 'linked' ? 'linked' : 'shared',
		account_label: connection.account_label,
		playlists: playlists.items.map(toSummary)
	};
}

async function fetchDetail(
	source: SourcePlaylistSource,
	playlistId: string,
	signal: AbortSignal
): Promise<SourcePlaylistDetail> {
	const detail = await api.v3.GET(
		v3('/api/v3/remotes/{source}/playlists/{id}', { path: { source, id: playlistId } }),
		{
			signal
		}
	);
	return {
		...toSummary(detail.playlist),
		tracks: detail.tracks.map((track) => ({
			id: track.id,
			track_name: track.title,
			artist_name: track.artist_name,
			album_name: track.album_name,
			album_id: track.album_id ?? '',
			artist_id: track.artist_id ?? undefined,
			plex_rating_key: source === 'plex' ? track.id : undefined,
			duration_seconds: track.duration_secs ?? 0,
			track_number: track.track_number ?? 0,
			disc_number: track.disc_number ?? 1,
			cover_url: track.image_url ?? ''
		}))
	};
}

export const getSourcePlaylistsQuery = (
	getSource: Getter<SourcePlaylistSource>,
	getLimit: Getter<number> = () => 200,
	getEnabled: Getter<boolean> = () => true
) =>
	createQuery(() => ({
		queryKey: SourcePlaylistQueryKeyFactory.list(authStore.user?.id, getSource(), getLimit()),
		queryFn: ({ signal }) => fetchCollection(getSource(), signal),
		enabled: getEnabled() && !!authStore.user?.id
	}));

export const getSourcePlaylistDetailQuery = (
	getSource: Getter<SourcePlaylistSource>,
	getPlaylistId: Getter<string>
) =>
	createQuery(() => ({
		queryKey: SourcePlaylistQueryKeyFactory.detail(
			authStore.user?.id,
			getSource(),
			getPlaylistId()
		),
		queryFn: ({ signal }) => fetchDetail(getSource(), getPlaylistId(), signal),
		enabled: !!authStore.user?.id && !!getPlaylistId()
	}));

// Copies one remote playlist into the caller's playlists. The new playlist
// shows up in the playlist list, so the playlist root is refreshed with the
// source's own lists.
export const createSourcePlaylistImportMutation = (getSource: () => SourcePlaylistSource) =>
	createAppMutation(() => ({
		mutationFn: (playlistId: string) =>
			api.v3.POST(
				v3('/api/v3/remotes/{source}/playlists/{id}/import', {
					path: { source: getSource(), id: playlistId }
				})
			),
		onMutate: () => ({ userId: authStore.user?.id }),
		effects: (_data, _playlistId, context) => {
			// A late answer after an account switch must not touch the new
			// user's cache.
			const userId = context.userId;
			if (!userId || authStore.user?.id !== userId) return [];
			const source = getSource();
			return [
				sourcePlaylistsChanged(userId, source),
				remoteSourceChanged(userId, source),
				playlistsChanged(userId),
				homeChanged(),
				anyLibraryChanged()
			];
		}
	}));
