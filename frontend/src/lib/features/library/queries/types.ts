import type { components } from '$lib/api/v3/openapi';

type Schemas = components['schemas'];

export type AlbumView = Schemas['AlbumView'];
export type AlbumDetailView = Schemas['AlbumDetailView'];
export type ArtistView = Schemas['ArtistView'];
export type TrackView = Schemas['TrackView'];
export type LibraryAlbumStatus = Schemas['LibraryAlbumStatus'];

export type ArtistSort = Schemas['ArtistListSort'];
/** The artist pages' two lists: album artists or contributors. */
export type LibraryArtistScope = Exclude<Schemas['ArtistListScope'], 'all'>;

export type ReleaseDraft = Schemas['ReleaseDraft'];
export type MusicBrainzSeed = Schemas['MusicBrainzSeed'];
export type LibraryContribution = Schemas['ContributionRecord'];

export type CrateReason = Schemas['SuggestionReason'];

/** A track on the local page's crate, from a search hit or a suggestion. */
export type CrateTrack = {
	track_file_id: string;
	title: string;
	album_name: string;
	artist_name: string;
	artist_id?: string | null;
	album_id: string;
	album_mbid?: string | null;
	cover_url?: string | null;
	format?: string;
	year?: number | null;
	duration_seconds?: number | null;
	reason: CrateReason;
};
