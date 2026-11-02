import type { components } from '$lib/api/v3/openapi';
import type {
	AlbumIdentityState,
	ContributionState,
	LibraryAlbumDetail,
	LibraryAlbumSummary,
	LibraryArtistRelationship,
	LibraryArtistSummary,
	NativeAlbumsResponse
} from '$lib/types';

export type AlbumView = components['schemas']['AlbumView'];
type AlbumDetailView = components['schemas']['AlbumDetailView'];
export type TrackView = components['schemas']['TrackView'];
type AlbumPage = components['schemas']['AlbumPage'];
type ArtistView = components['schemas']['ArtistView'];

// The library pages read the catalog views under the names the screens use.

function albumIdentity(album: AlbumView): AlbumIdentityState {
	if (album.identity_state !== 'linked') return 'local_only';
	return album.release_mbid ? 'release_linked' : 'release_group_linked';
}

export function toAlbumSummary(album: AlbumView): LibraryAlbumSummary {
	return {
		id: album.id,
		title: album.title,
		artist_name: album.artist_name,
		artist_id: album.artist_id,
		musicbrainz_release_group_id: album.release_group_mbid ?? null,
		musicbrainz_release_id: album.release_mbid ?? null,
		musicbrainz_artist_id: album.artist_mbid ?? null,
		album_identity_state: albumIdentity(album),
		track_count: album.track_count,
		total_duration_seconds: album.total_duration_seconds,
		total_size_bytes: album.total_size_bytes,
		format: album.format ?? null,
		year: album.year ?? null,
		is_compilation: album.is_compilation,
		cover_available: album.cover_available,
		art_missing: album.art_missing,
		date_added: album.date_added ?? null,
		contribution_id: album.contribution_id ?? null,
		contribution_state: (album.contribution_state as ContributionState | null | undefined) ?? null,
		favorite: album.favorite
	};
}

export function toAlbumDetail(album: AlbumDetailView): LibraryAlbumDetail {
	return {
		...toAlbumSummary(album),
		row_revision: album.row_revision,
		review_id: album.pending_review_id ?? null,
		management_hold: album.management_hold ?? null
	};
}

export function toNativeAlbums(page: AlbumPage): NativeAlbumsResponse {
	return { items: page.items.map(toAlbumSummary), total: page.total };
}

function relationship(artist: ArtistView): LibraryArtistRelationship {
	if (artist.album_count > 0 && artist.appearance_album_count > 0) return 'both';
	return artist.album_count > 0 ? 'album_artist' : 'contributor';
}

export function toArtistSummary(artist: ArtistView): LibraryArtistSummary {
	return {
		id: artist.id,
		name: artist.name,
		musicbrainz_artist_id: artist.artist_mbid ?? null,
		artist_identity_state: artist.identity_state === 'linked' ? 'musicbrainz_linked' : 'local_only',
		album_count: artist.album_count,
		track_count: artist.led_track_count,
		appearance_release_count: artist.appearance_album_count,
		appearance_track_count: artist.appearance_track_count,
		library_relationship: relationship(artist),
		date_added: artist.date_added ?? null,
		art_missing: artist.art_missing,
		favorite: artist.favorite
	};
}
