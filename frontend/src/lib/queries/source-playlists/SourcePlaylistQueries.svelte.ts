import { createQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { REMOTE_ENDPOINTS } from '$lib/queries/remotes/endpoints';
import { authStore } from '$lib/stores/authStore.svelte';
import type {
	SourcePlaylistCollection,
	SourcePlaylistDetail,
	SourcePlaylistSource,
	SourcePlaylistSummary
} from '$lib/types';

import { SourcePlaylistQueryKeyFactory } from './SourcePlaylistQueryKeyFactory';

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
	limit: number,
	signal: AbortSignal
): Promise<SourcePlaylistCollection> {
	const [playlists, connection] = await Promise.all([
		api.global.v3.GET(REMOTE_ENDPOINTS.playlists(source, { limit }), { signal }),
		api.global.v3.GET(REMOTE_ENDPOINTS.connection(source), { signal })
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
	const detail = await api.global.v3.GET(REMOTE_ENDPOINTS.playlist(source, playlistId), {
		signal
	});
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
		queryFn: ({ signal }) => fetchCollection(getSource(), getLimit(), signal),
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
