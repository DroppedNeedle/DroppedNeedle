import type { components } from '$lib/api/v3/openapi';

type Schemas = components['schemas'];

export type AlbumView = Schemas['AlbumView'];
export type AlbumDetailView = Schemas['AlbumDetailView'];
export type ArtistView = Schemas['ArtistView'];
export type TrackView = Schemas['TrackView'];
export type LibraryAlbumStatus = Schemas['LibraryAlbumStatus'];

export type ArtistSort = 'name' | 'album_count' | 'appearance_count' | 'date_added';
export type LibraryArtistScope = 'album' | 'contributors';

export type ReleaseDraft = Schemas['ReleaseDraft'];
export type MusicBrainzSeed = Schemas['MusicBrainzSeed'];
export type LibraryContribution = Schemas['ContributionRecord'];

// The suggestion's reason is plain text in the contract; the crate knows these.
export type CrateReason = 'recent' | 'rediscover' | 'surprise' | 'same_era';

/** A track on the local page's crate, from a search hit or a suggestion. */
export type CrateTrack = {
	track_file_id: string;
	title: string;
	album_name: string;
	artist_name: string;
	album_id: string;
	album_mbid?: string | null;
	cover_url?: string | null;
	format?: string;
	year?: number | null;
	duration_seconds?: number | null;
	reason: CrateReason;
};
