import type { components } from '$lib/api/v3/openapi';
import type {
	PlaylistDetail,
	PlaylistListItem,
	PlaylistSummary,
	PlaylistTrack,
	RedactedPlaylist,
	TrackData
} from '$lib/api/playlists';

export type PlaylistSummaryV3 = components['schemas']['PlaylistSummary'];
export type PlaylistDetailV3 = components['schemas']['PlaylistDetail'];
export type PlaylistTrackV3 = components['schemas']['PlaylistTrack'];
export type PlaylistListItemV3 = components['schemas']['PlaylistListItem'];
export type TrackInputV3 = components['schemas']['TrackInput'];
export type CheckTracksResponseV3 = components['schemas']['CheckTracksResponse'];

/** V3 epoch seconds to the ISO strings the playlist pages render. */
export function epochSecToIso(epochSec: number): string {
	return new Date(epochSec * 1000).toISOString();
}

/** Narrow a V3 list row to its redacted stub. */
export function isRedactedListItemV3(
	item: PlaylistListItemV3
): item is components['schemas']['RedactedPlaylist'] {
	return item.is_redacted === true;
}

/** V3 track row to the page track shape. */
export function toPageTrack(track: PlaylistTrackV3): PlaylistTrack {
	return {
		id: track.id,
		position: track.position,
		track_name: track.track_name,
		artist_name: track.artist_name,
		album_name: track.album_name,
		album_id: track.album_id ?? null,
		artist_id: track.artist_id ?? null,
		track_source_id: track.track_source_id ?? null,
		cover_url: track.cover_url ?? null,
		source_type: track.source_type,
		available_sources: track.available_sources ?? null,
		format: track.format ?? null,
		track_number: track.track_number ?? null,
		disc_number: track.disc_number ?? null,
		duration: track.duration ?? null,
		created_at: epochSecToIso(track.created_at),
		plex_rating_key: track.plex_rating_key ?? null,
		library_file_id: track.library_file_id ?? null
	};
}

/** V3 summary to the page summary shape (provenance passes through). */
export function toPageSummary(summary: PlaylistSummaryV3): PlaylistSummary {
	return {
		id: summary.id,
		name: summary.name,
		track_count: summary.track_count,
		total_duration: summary.total_duration ?? null,
		cover_urls: summary.cover_urls,
		custom_cover_url: summary.custom_cover_url ?? null,
		source_ref: summary.source_ref ?? null,
		created_at: epochSecToIso(summary.created_at),
		updated_at: epochSecToIso(summary.updated_at),
		is_public: summary.is_public,
		is_owner: summary.is_owner,
		owner_name: summary.owner_name ?? null,
		is_redacted: false
	};
}

/** V3 detail to the page detail shape. */
export function toPageDetail(detail: PlaylistDetailV3): PlaylistDetail {
	return {
		...toPageSummary(detail),
		tracks: detail.tracks.map(toPageTrack)
	};
}

/** V3 list row to the page list row (redacted stubs pass through untouched). */
export function toPageListItem(item: PlaylistListItemV3): PlaylistListItem {
	if (isRedactedListItemV3(item)) {
		const redacted: RedactedPlaylist = {
			id: item.id,
			track_count: item.track_count,
			owner_name: item.owner_name ?? null,
			is_redacted: true
		};
		return redacted;
	}
	return toPageSummary(item);
}

/** V3 list answer to page rows. */
export function toPageList(items: PlaylistListItemV3[]): PlaylistListItem[] {
	return items.map(toPageListItem);
}

/** Page track payload to the V3 add-tracks input. */
export function trackDataToV3Input(data: TrackData): TrackInputV3 {
	return {
		track_name: data.track_name,
		artist_name: data.artist_name,
		album_name: data.album_name,
		album_id: data.album_id ?? null,
		artist_id: data.artist_id ?? null,
		track_source_id: data.track_source_id ?? null,
		cover_url: data.cover_url ?? null,
		source_type: data.source_type,
		available_sources: data.available_sources ?? null,
		format: data.format ?? null,
		track_number: data.track_number ?? null,
		disc_number: data.disc_number ?? null,
		duration: data.duration ?? null,
		plex_rating_key: data.plex_rating_key ?? null
	};
}

/**
 * V3 membership (`track index -> playlist ids`) to the page shape
 * (`playlist id -> track indices`).
 */
export function transposeMembership(
	membership: CheckTracksResponseV3['membership']
): Record<string, number[]> {
	const transposed: Record<string, number[]> = {};
	for (const [index, playlistIds] of Object.entries(membership)) {
		const trackIndex = Number(index);
		if (!Number.isInteger(trackIndex)) continue;
		for (const playlistId of playlistIds) {
			const existing = transposed[playlistId];
			if (existing) {
				existing.push(trackIndex);
			} else {
				transposed[playlistId] = [trackIndex];
			}
		}
	}
	return transposed;
}
