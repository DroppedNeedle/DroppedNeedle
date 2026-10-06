import { api } from '$lib/api/client';
import type { QueueItem } from '$lib/player/types';
import { PlaylistV3Api } from '$lib/queries/playlists/PlaylistV3Api';
import { REQUESTS_ENDPOINTS } from '$lib/queries/requests/endpoints';
import { toPageDetail } from '$lib/queries/playlists/playlistV3Adapter';

export interface PlaylistTrack {
	id: string;
	position: number;
	track_name: string;
	artist_name: string;
	album_name: string;
	album_id: string | null;
	artist_id: string | null;
	track_source_id: string | null;
	cover_url: string | null;
	source_type: string;
	available_sources: string[] | null;
	format: string | null;
	track_number: number | null;
	disc_number: number | null;
	duration: number | null;
	created_at: string;
	plex_rating_key: string | null;
	library_file_id: string | null;
}

export interface PlaylistSummary {
	id: string;
	name: string;
	track_count: number;
	total_duration: number | null;
	cover_urls: string[];
	custom_cover_url: string | null;
	source_ref: string | null;
	created_at: string;
	updated_at: string;
	// Ownership / visibility (D4).
	is_public: boolean;
	is_owner: boolean;
	owner_name: string | null;
	is_redacted: false;
}

export interface PlaylistDetail extends PlaylistSummary {
	tracks: PlaylistTrack[];
}

/** Admin's view of another user's PRIVATE playlist (D4): count + owner only. */
export interface RedactedPlaylist {
	id: string;
	track_count: number;
	owner_name: string | null;
	is_redacted: true;
}

export type PlaylistListItem = PlaylistSummary | RedactedPlaylist;
export type PlaylistDetailItem = PlaylistDetail | RedactedPlaylist;

export function isRedactedPlaylist(
	item: PlaylistListItem | PlaylistDetailItem
): item is RedactedPlaylist {
	return item.is_redacted === true;
}

export interface TrackData {
	track_name: string;
	artist_name: string;
	album_name: string;
	album_id?: string | null;
	artist_id?: string | null;
	track_source_id?: string | null;
	cover_url?: string | null;
	source_type: string;
	available_sources?: string[] | null;
	format?: string | null;
	track_number?: number | null;
	disc_number?: number | null;
	duration?: number | null;
	plex_rating_key?: string | null;
}

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
		plex_rating_key: item.plexRatingKey ?? null
	};
}

// Thin helpers over the v3 contract: every call below hits /api/v3 and adapts
// the wire shape (epoch dates, optional provenance) to the page shapes above.

export async function fetchPlaylist(
	id: string,
	options?: { signal?: AbortSignal }
): Promise<PlaylistDetailItem> {
	const data = await api.global.v3.GET(PlaylistV3Api.detail(id), { signal: options?.signal });
	return toPageDetail(data);
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
	for (const track of tracks) {
		const mbid = track.album_id;
		if (!mbid || seen.has(mbid)) continue;
		if (track.library_file_id) continue;
		if (track.available_sources && track.available_sources.length > 0) continue;
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
	const data = await api.global.v3.POST(REQUESTS_ENDPOINTS.requestBatch(), { items });
	return {
		success: data.success,
		message: data.message,
		requested: data.requested,
		skipped: data.skipped
	};
}
