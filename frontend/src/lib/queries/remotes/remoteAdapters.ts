import type { components } from '$lib/api/v3/openapi';
import type {
	JellyfinAlbumSummary,
	JellyfinArtistSummary,
	JellyfinTrackInfo,
	NavidromeAlbumSummary,
	NavidromeArtistSummary,
	NavidromeTrackInfo,
	PlexAlbumSummary,
	PlexArtistSummary,
	PlexTrackInfo
} from '$lib/types';

// v3 serves one album, artist and track view for every remote source. The
// library pages, the album modal and the player helpers work on per-source
// page shapes (each carries its own id field), so the views are adapted
// here, once, at the query edge.

type AlbumView = components['schemas']['RemotesAlbumView'];
type ArtistView = components['schemas']['RemotesArtistView'];
type TrackView = components['schemas']['RemotesTrackView'];

export function toJellyfinTrack(track: TrackView): JellyfinTrackInfo {
	return {
		jellyfin_id: track.id,
		title: track.title,
		track_number: track.track_number ?? 0,
		disc_number: track.disc_number ?? null,
		duration_seconds: track.duration_secs ?? 0,
		album_name: track.album_name,
		artist_name: track.artist_name,
		album_id: track.album_id ?? undefined,
		image_url: track.image_url ?? null
	};
}

export function toNavidromeTrack(track: TrackView): NavidromeTrackInfo {
	return {
		navidrome_id: track.id,
		title: track.title,
		track_number: track.track_number ?? 0,
		disc_number: track.disc_number ?? null,
		duration_seconds: track.duration_secs ?? 0,
		album_name: track.album_name,
		artist_name: track.artist_name,
		image_url: track.image_url ?? null
	};
}

export function toPlexTrack(track: TrackView): PlexTrackInfo {
	return {
		plex_id: track.id,
		title: track.title,
		track_number: track.track_number ?? 0,
		disc_number: track.disc_number ?? 1,
		duration_seconds: track.duration_secs ?? 0,
		album_name: track.album_name,
		artist_name: track.artist_name,
		part_key: track.part_key ?? null,
		image_url: track.image_url ?? null
	};
}

function albumFields(album: AlbumView) {
	return {
		name: album.title,
		artist_name: album.artist_name,
		year: album.year ?? null,
		track_count: album.track_count ?? 0,
		image_url: album.image_url ?? null,
		musicbrainz_id: album.release_group_mbid ?? null,
		artist_musicbrainz_id: album.artist_mbid ?? null
	};
}

export function toJellyfinAlbum(album: AlbumView): JellyfinAlbumSummary {
	return { jellyfin_id: album.id, ...albumFields(album) };
}

export function toNavidromeAlbum(album: AlbumView): NavidromeAlbumSummary {
	return { navidrome_id: album.id, ...albumFields(album) };
}

export function toPlexAlbum(album: AlbumView): PlexAlbumSummary {
	return { plex_id: album.id, ...albumFields(album) };
}

export function toJellyfinArtist(artist: ArtistView): JellyfinArtistSummary {
	return {
		jellyfin_id: artist.id,
		name: artist.name,
		image_url: artist.image_url ?? null,
		album_count: artist.album_count ?? 0,
		musicbrainz_id: artist.artist_mbid ?? null
	};
}

export function toNavidromeArtist(artist: ArtistView): NavidromeArtistSummary {
	return {
		navidrome_id: artist.id,
		name: artist.name,
		image_url: artist.image_url ?? null,
		album_count: artist.album_count ?? 0,
		musicbrainz_id: artist.artist_mbid ?? null
	};
}

export function toPlexArtist(artist: ArtistView): PlexArtistSummary {
	return {
		plex_id: artist.id,
		name: artist.name,
		image_url: artist.image_url ?? null,
		musicbrainz_id: artist.artist_mbid ?? null
	};
}
