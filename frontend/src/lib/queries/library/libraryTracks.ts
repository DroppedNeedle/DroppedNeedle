import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import type { LibraryAlbumStatus, NativeTrackListItem, NativeTrackPage } from '$lib/types';
import { LibraryV3Api } from './LibraryV3Api';

/** Track list orders the v3 catalog serves. */
export type LibraryTrackSort = 'recent' | 'title';

// The local track list and the player helpers read the native track page
// model. The quality tier and the upgrade-cutoff flag only come with the
// album status view (toAlbumStatus); plain track reads leave them unset.
export function toNativeTrack(track: components['schemas']['TrackView']): NativeTrackListItem {
	return {
		id: track.id,
		title: track.title,
		album_id: track.album_id,
		album_title: track.album_title,
		artist_id: track.artist_id ?? '',
		artist_name: track.artist_name,
		album_artist_id: track.album_artist_id ?? '',
		album_artist_name: track.album_artist_name,
		musicbrainz_recording_id: track.recording_mbid ?? null,
		musicbrainz_release_group_id: track.release_group_mbid ?? null,
		musicbrainz_artist_id: track.artist_mbid ?? null,
		musicbrainz_album_artist_id: track.album_artist_mbid ?? null,
		disc_number: track.disc_number,
		track_number: track.track_number,
		year: track.year ?? null,
		genre: track.genre ?? null,
		duration_seconds: track.duration_seconds ?? 0,
		format: track.format,
		bit_rate: track.bit_rate ?? null,
		sample_rate: track.sample_rate ?? null,
		bit_depth: track.bit_depth ?? null,
		channels: track.channels ?? null,
		file_size_bytes: track.file_size_bytes,
		date_added: track.date_added ?? null,
		cover_available: track.cover_available,
		current_tier: null,
		below_cutoff: false
	};
}

/** The album page's library status, tracks in the native track model. */
export function toAlbumStatus(
	status: components['schemas']['LibraryAlbumStatus']
): LibraryAlbumStatus {
	return {
		in_library: status.in_library,
		album_id: status.album_id,
		track_count: status.track_count,
		tracks: status.tracks.map((track) => ({
			...toNativeTrack(track),
			current_tier: track.current_tier,
			below_cutoff: track.below_cutoff
		}))
	};
}

/** One page of the local catalog's tracks, newest first or by title. */
export async function fetchLibraryTracks(
	limit: number,
	offset: number,
	sort: LibraryTrackSort,
	q: string = '',
	signal?: AbortSignal
): Promise<NativeTrackPage> {
	const page = await api.global.v3.GET(
		LibraryV3Api.tracks({
			limit,
			offset,
			sort: sort === 'recent' ? 'date_added' : 'title',
			order: sort === 'recent' ? 'desc' : 'asc',
			q: q || undefined
		}),
		{ signal }
	);
	return { ...page, items: page.items.map(toNativeTrack) };
}
