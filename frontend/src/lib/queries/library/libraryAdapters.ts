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

// The library pages read the native page models that the v2 catalog served.
// v3's catalog views carry the browse fields plus the open contribution:
// identity is `local_only` or `linked`, and the management and
// edition-conversion fields have no v3 source yet, so they read as
// "nothing to manage" here.

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
		release_type: null,
		cover_available: album.cover_available,
		art_missing: album.art_missing,
		date_added: album.date_added ?? null,
		sort_name: null,
		original_release_date: null,
		contribution_id: album.contribution_id ?? null,
		contribution_state: (album.contribution_state as ContributionState | null | undefined) ?? null,
		favorite: album.favorite
	};
}

export function toAlbumDetail(album: AlbumDetailView): LibraryAlbumDetail {
	return {
		...toAlbumSummary(album),
		row_revision: 0,
		input_revision: '',
		identification_status: album.identity_state === 'linked' ? 'identified' : 'local_metadata',
		review_id: null,
		review_revision: null,
		management_identity_readiness: 'not_applicable',
		mapped_track_count: 0,
		management_identity_kind: null,
		custom_manifest_id: null,
		custom_manifest_version: null,
		custom_manifest_track_count: 0,
		custom_manifest_recognized_track_count: 0,
		custom_manifest_stale: false,
		management_hold: album.management_hold ?? null,
		active_edition_conversion: null,
		display_release_mbid: album.release_mbid ?? null,
		pick_basis: null
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
		track_count: artist.track_count,
		appearance_release_count: artist.appearance_album_count,
		appearance_track_count: artist.appearance_track_count,
		library_relationship: relationship(artist),
		date_added: artist.date_added ?? null,
		row_revision: 0,
		art_missing: artist.art_missing,
		favorite: artist.favorite
	};
}
