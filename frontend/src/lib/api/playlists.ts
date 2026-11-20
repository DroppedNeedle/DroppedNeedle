import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import type { QueueItem } from '$lib/features/player/playback/types';
import type { components } from '$lib/api/v3/openapi';
import { toPageDetail } from '$lib/features/playlists/queries/playlistV3Adapter';
import { downloadBlob } from '$lib/utils/blobDownload';

type Schemas = components['schemas'];

// The contract's rows with dates as ISO strings and every optional field filled.
export type PlaylistTrack = Required<Omit<Schemas['PlaylistTrack'], 'created_at'>> & {
	created_at: string;
};

export type PlaylistSummary = Required<
	Omit<Schemas['PlaylistSummary'], 'created_at' | 'updated_at' | 'is_redacted'>
> & {
	created_at: string;
	updated_at: string;
	is_redacted: false;
};

export type PlaylistDetail = PlaylistSummary & { tracks: PlaylistTrack[] };

/** A private playlist the caller may not read: count + owner only. */
export type RedactedPlaylist = Required<Omit<Schemas['RedactedPlaylist'], 'is_redacted'>> & {
	is_redacted: true;
};

export type PlaylistListItem = PlaylistSummary | RedactedPlaylist;
export type PlaylistDetailItem = PlaylistDetail | RedactedPlaylist;

export function isRedactedPlaylist(
	item: PlaylistListItem | PlaylistDetailItem
): item is RedactedPlaylist {
	return item.is_redacted === true;
}

export type TrackData = Schemas['TrackInput'] & { source_type: string };

export function queueItemToTrackData(item: QueueItem): TrackData {
	return {
		track_name: item.trackName,
		artist_name: item.artistName,
		album_name: item.albumName,
		album_id: item.albumId || null,
		artist_id: item.artistId || null,
		track_source_id: item.trackSourceId || null,
		cover_url: item.coverUrl,
		source_type: item.sourceType,
		available_sources: item.availableSources ?? null,
		format: item.format ?? null,
		track_number: item.trackNumber ?? null,
		disc_number: item.discNumber ?? null,
		duration: item.duration ?? null,
		plex_rating_key: item.plexRatingKey ?? null,
		recording_mbid: item.recordingIds?.[item.sourceType] ?? null
	};
}

// Thin helpers over the v3 contract: every call below hits /api/v3 and adapts
// the wire shape (epoch dates, optional provenance) to the page shapes above.

export async function fetchPlaylist(
	id: string,
	options?: { signal?: AbortSignal }
): Promise<PlaylistDetailItem> {
	const data = await api.v3.GET(
		v3('/api/v3/playlists/{playlist_id}', { path: { playlist_id: id } }),
		{ signal: options?.signal }
	);
	return toPageDetail(data);
}

/** A playlist export file format. */
export type PlaylistExportFormat = 'm3u8' | 'csv';

/**
 * Download one playlist as M3U8 or CSV. `prefix` goes before each path, for
 * a player that stores music under a folder. The server names the file and
 * leaves out entries with no local file; returns how many it left out.
 */
export async function downloadPlaylistExport(
	id: string,
	format: PlaylistExportFormat,
	prefix?: string
): Promise<number> {
	const trimmed = prefix?.trim() ?? '';
	const response = await downloadBlob(
		v3('/api/v3/playlists/{playlist_id}/export', {
			path: { playlist_id: id },
			query: { format, ...(trimmed ? { prefix: trimmed } : {}) }
		})
	);
	const skipped = Number.parseInt(response.headers.get('x-droppedneedle-skipped-tracks') ?? '', 10);
	return Number.isNaN(skipped) ? 0 : skipped;
}

export interface BatchRequestResult {
	success: boolean;
	message: string;
	requested: number;
	skipped: number;
}

export async function requestMissingTracks(tracks: PlaylistTrack[]): Promise<BatchRequestResult> {
	// v1 parity (request_missing_tracks): one album row per unresolved
	// album, deduped by album id, skipping library-resolved and sourced
	// tracks. Posts to the v3 batch intake, whose response extends this
	// shape with overflow + status.
	const seen = new Set<string>();
	const items: { musicbrainz_id: string; artist_name: string; album_title: string }[] = [];
	// Missing songs with no MusicBrainz album have nothing to request yet.
	let noAlbum = 0;
	for (const track of tracks) {
		if (track.library_file_id) continue;
		if (track.available_sources && track.available_sources.length > 0) continue;
		const mbid = track.album_id;
		if (!mbid) {
			noAlbum += 1;
			continue;
		}
		if (seen.has(mbid)) continue;
		seen.add(mbid);
		items.push({
			musicbrainz_id: mbid,
			artist_name: track.artist_name || 'Unknown',
			album_title: track.album_name || 'Unknown'
		});
	}
	if (items.length === 0) {
		return {
			success: true,
			message: 'No missing albums found, all tracks already have a source',
			requested: 0,
			skipped: 0
		};
	}
	const data = await api.v3.POST(v3('/api/v3/requests/batches'), { items });
	return {
		success: data.success,
		message:
			noAlbum === 0
				? data.message
				: `${data.message.replace(/\.$/, '')}. ${noAlbum} ${noAlbum === 1 ? 'song has' : 'songs have'} no album to request yet.`,
		requested: data.requested,
		skipped: data.skipped
	};
}
