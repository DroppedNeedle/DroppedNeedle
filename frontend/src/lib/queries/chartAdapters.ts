import type { components } from '$lib/api/v3/openapi';
import type {
	DiscoverPreview,
	GenreDetailResponse,
	GenreArtwork,
	GenreArtworkAlbum,
	HomeAlbum,
	HomeArtist,
	HomeGenre,
	HomeResponse,
	HomeSection,
	HomeTrack,
	ServicePrompt,
	WeeklyExplorationSection,
	WeeklyExplorationTrack
} from '$lib/types';

type V3Section = components['schemas']['ChartSection'];
type V3Item = components['schemas']['SectionItem'];
type V3Artist = components['schemas']['ChartArtist'];
type V3Album = components['schemas']['ChartAlbum'];
type V3Track = components['schemas']['ChartTrack'];
type V3Genre = components['schemas']['ChartGenre'];
type V3Weekly = components['schemas']['WeeklyExploration'];
type V3WeeklyTrack = components['schemas']['WeeklyTrack'];
type V3Prompt = components['schemas']['ServicePrompt'];
type V3Artwork = components['schemas']['GenreArtwork'];
type V3ArtworkAlbum = components['schemas']['GenreArtworkAlbum'];
type V3Integration = components['schemas']['IntegrationStatus'];
type V3Home = components['schemas']['HomeResponse'];
type V3DiscoverPreview = components['schemas']['DiscoverPreview'];
type V3GenreDetail = components['schemas']['GenreDetailResponse'];

// Adapters from the v3 chart shelves (home, discover, genre) to the page
// shapes the shelves render. The contract marks most fields optional and
// leaves section rows untagged; the page shapes fill the defaults once and
// narrow each row by its section type, so the shelves stay simple.
const SECTION_TYPES = ['artists', 'albums', 'tracks', 'genres'] as const;

type SectionType = (typeof SECTION_TYPES)[number];

function sectionTypeOf(raw: string): SectionType {
	return (SECTION_TYPES as readonly string[]).includes(raw) ? (raw as SectionType) : 'albums';
}

export function toArtist(item: V3Artist): HomeArtist {
	return {
		mbid: item.mbid ?? null,
		local_id: item.local_id ?? null,
		name: item.name,
		image_url: item.image_url ?? null,
		listen_count: item.listen_count ?? null,
		in_library: item.in_library ?? false
	};
}

export function toAlbum(item: V3Album): HomeAlbum {
	return {
		mbid: item.mbid ?? null,
		local_id: item.local_id ?? null,
		name: item.name,
		artist_name: item.artist_name ?? null,
		artist_mbid: item.artist_mbid ?? null,
		image_url: item.image_url ?? null,
		release_date: item.release_date ?? null,
		listen_count: item.listen_count ?? null,
		in_library: item.in_library ?? false,
		requested: item.requested ?? undefined
	};
}

function toTrack(item: V3Track): HomeTrack {
	return {
		mbid: item.mbid ?? null,
		name: item.name,
		artist_name: item.artist_name ?? null,
		artist_mbid: item.artist_mbid ?? null,
		album_name: item.album_name ?? null,
		listen_count: item.listen_count ?? null,
		listened_at: item.listened_at ?? null,
		image_url: item.image_url ?? null
	};
}

function toGenre(item: V3Genre): HomeGenre {
	return {
		name: item.name,
		listen_count: item.listen_count ?? null,
		artist_count: item.artist_count ?? null,
		artist_mbid: item.artist_mbid ?? null
	};
}

// Section rows ride untagged on the wire; the section type names the
// variant, exactly how the shelves below read them.
function toItems(type: SectionType, items: V3Item[] | undefined): HomeSection['items'] {
	if (!items) return [];
	switch (type) {
		case 'artists':
			return items.map((item) => toArtist(item as V3Artist));
		case 'tracks':
			return items.map((item) => toTrack(item as V3Track));
		case 'genres':
			return items.map((item) => toGenre(item as V3Genre));
		case 'albums':
		default:
			return items.map((item) => toAlbum(item as V3Album));
	}
}

export function toHomeSection(section: V3Section): HomeSection {
	const type = sectionTypeOf(section.type);
	return {
		title: section.title,
		type,
		items: toItems(type, section.items),
		source: section.source ?? null,
		fallback_message: section.fallback_message ?? null,
		connect_service: section.connect_service ?? null,
		radio_seed_type: section.radio_seed_type ?? null,
		radio_seed_id: section.radio_seed_id ?? null
	};
}

function toWeeklyTrack(track: V3WeeklyTrack): WeeklyExplorationTrack {
	return {
		title: track.title,
		artist_name: track.artist_name,
		album_name: track.album_name,
		recording_mbid: track.recording_mbid ?? null,
		artist_mbid: track.artist_mbid ?? null,
		release_group_mbid: track.release_group_mbid ?? null,
		cover_url: track.cover_url ?? null,
		duration_ms: track.duration_ms ?? null
	};
}

export function toWeekly(section: V3Weekly): WeeklyExplorationSection {
	return {
		title: section.title,
		playlist_date: section.playlist_date,
		tracks: (section.tracks ?? []).map(toWeeklyTrack),
		source_url: section.source_url ?? ''
	};
}

export function toPrompt(prompt: V3Prompt): ServicePrompt {
	return {
		service: prompt.service,
		title: prompt.title,
		description: prompt.description,
		icon: prompt.icon,
		color: prompt.color,
		features: prompt.features ?? []
	};
}

function toArtworkAlbum(album: V3ArtworkAlbum): GenreArtworkAlbum {
	return {
		album_id: album.album_id,
		album_title: album.album_title,
		album_artist_name: album.album_artist_name ?? null,
		cover_version: album.cover_version
	};
}

export function toArtwork(artwork: V3Artwork): GenreArtwork {
	return {
		kind: artwork.kind === 'gradient' ? 'gradient' : 'collage',
		albums: (artwork.albums ?? []).map(toArtworkAlbum),
		version: artwork.version
	};
}

export function toIntegrationStatus(
	status: V3Integration | null | undefined
): Record<string, boolean> {
	if (!status) return {};
	const out: Record<string, boolean> = {};
	for (const [key, value] of Object.entries(status)) {
		if (typeof value === 'boolean') out[key] = value;
	}
	return out;
}

export const maybe = <T, U>(value: T | null | undefined, map: (v: T) => U): U | null =>
	value === null || value === undefined ? null : map(value);

export function toGenreArtworkMap(
	artwork: Record<string, V3Artwork> | undefined
): Record<string, GenreArtwork> {
	return Object.fromEntries(
		Object.entries(artwork ?? {}).map(([genre, entry]) => [genre, toArtwork(entry)])
	);
}

function toDiscoverPreview(preview: V3DiscoverPreview): DiscoverPreview {
	return {
		seed_artist: preview.seed_artist,
		seed_artist_mbid: preview.seed_artist_mbid,
		items: (preview.items ?? []).map(toArtist)
	};
}

export function toHomeResponse(home: V3Home): HomeResponse {
	return {
		refreshing: home.refreshing ?? false,
		recently_added: maybe(home.recently_added, toHomeSection),
		library_artists: maybe(home.library_artists, toHomeSection),
		library_albums: maybe(home.library_albums, toHomeSection),
		recommended_artists: maybe(home.recommended_artists, toHomeSection),
		trending_artists: maybe(home.trending_artists, toHomeSection),
		popular_albums: maybe(home.popular_albums, toHomeSection),
		recently_played: maybe(home.recently_played, toHomeSection),
		top_genres: maybe(home.top_genres, toHomeSection),
		genre_list: maybe(home.genre_list, toHomeSection),
		fresh_releases: maybe(home.fresh_releases, toHomeSection),
		favorite_artists: maybe(home.favorite_artists, toHomeSection),
		your_top_albums: maybe(home.your_top_albums, toHomeSection),
		weekly_exploration: maybe(home.weekly_exploration, toWeekly),
		service_prompts: (home.service_prompts ?? []).map(toPrompt),
		integration_status: toIntegrationStatus(home.integration_status),
		genre_artwork: toGenreArtworkMap(home.genre_artwork),
		genre_artwork_schema_version: 'v2',
		discover_preview: maybe(home.discover_preview, toDiscoverPreview)
	};
}

export function toGenreDetail(detail: V3GenreDetail): GenreDetailResponse {
	return {
		genre: detail.genre,
		genre_artwork: toArtwork(detail.genre_artwork),
		library: maybe(detail.library, (library) => ({
			artists: (library.artists ?? []).map(toArtist),
			albums: (library.albums ?? []).map(toAlbum),
			artist_count: library.artist_count,
			album_count: library.album_count
		})),
		popular: maybe(detail.popular, (popular) => ({
			artists: (popular.artists ?? []).map(toArtist),
			albums: (popular.albums ?? []).map(toAlbum),
			has_more_artists: popular.has_more_artists,
			has_more_albums: popular.has_more_albums
		})),
		artists: (detail.artists ?? []).map(toArtist),
		total_count: detail.total_count ?? null
	};
}
