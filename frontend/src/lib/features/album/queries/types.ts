import type { components } from '$lib/api/v3/openapi';
import type { HeldEntry, HeldFile } from '$lib/features/downloads/queries/types';

type Schemas = components['schemas'];

/** An album card: a search hit, a shelf item or a library album. */
export type Album = {
	title: string;
	artist: string | null;
	year: number | null;
	musicbrainz_id: string;
	in_library: boolean;
	requested?: boolean;
	cover_url?: string | null;
	album_thumb_url?: string | null;
	type_info?: string | null;
	disambiguation?: string | null;
	track_count?: number | null;
	listen_count?: number | null;
	score?: number;
	local_id?: string | null;
	art_missing?: boolean;
};

/** The provider behind an enrichment batch; `null` when none answered. */
export type EnrichmentSource = Schemas['ListenSource'] | null;

export type ReleaseGroup = Omit<Schemas['ReleaseItem'], 'title'> & { title: string };

export type ArtistInfoBasic = Omit<
	Schemas['ArtistInfo'],
	'albums' | 'singles' | 'eps' | 'source' | 'images'
> &
	Schemas['ArtistImages'];

export type ArtistReleases = Omit<
	Schemas['ArtistReleases'],
	'albums' | 'singles' | 'eps' | 'source'
> & {
	albums: ReleaseGroup[];
	singles: ReleaseGroup[];
	eps: ReleaseGroup[];
};

export type Track = Schemas['AlbumTrack'];

// The library's own album pages build one from a local album, which has no
// provider source or request state.
export type AlbumBasicInfo = Omit<
	Schemas['AlbumBasicInfo'],
	'requested' | 'service_status' | 'source'
> &
	Partial<Pick<Schemas['AlbumBasicInfo'], 'requested' | 'service_status'>>;

export type AlbumTracksInfo = Schemas['AlbumTracksInfo'];

/** A held track with its quality tier against the upgrade cutoff. */
export type LibraryStatusTrack = Schemas['LibraryStatusTrack'];

export type LocalTrackInfo = {
	track_file_id: string;
	/** What removing this file sends back. Absent where the view offers no removal. */
	row_revision?: Schemas['TrackView']['row_revision'];
	title: string;
	track_number: number;
	disc_number?: number | null;
	duration_seconds?: number | null;
	size_bytes: number;
	format: string;
	bitrate?: number | null;
};

export type LocalAlbumMatch = {
	found: boolean;
	musicbrainz_id?: string | null;
	tracks: LocalTrackInfo[];
	total_size_bytes: number;
	primary_format?: string | null;
};

/** A held file with the entry it belongs to, as an album page's track row shows it. */
export interface HeldFileInEntry {
	entry: HeldEntry;
	file: HeldFile;
}
