import type { components } from '$lib/api/v3/openapi';
import type {
	BecauseYouListenTo,
	DiscoverResponse,
	GenreArtwork,
	GenreArtworkAlbum,
	HomeAlbum,
	HomeArtist,
	HomeGenre,
	HomeSection,
	HomeTrack,
	ServicePrompt,
	TopPicksSection,
	WeeklyExplorationSection,
	WeeklyExplorationTrack
} from '$lib/types';

type V3Response = components['schemas']['DiscoverResponse'];
type V3Section = components['schemas']['ChartSection'];
type V3Item = components['schemas']['SectionItem'];
type V3Artist = components['schemas']['ChartArtist'];
type V3Album = components['schemas']['ChartAlbum'];
type V3Track = components['schemas']['ChartTrack'];
type V3Genre = components['schemas']['ChartGenre'];
type V3TopPicks = components['schemas']['TopPicksSection'];
type V3Because = components['schemas']['BecauseYouListenTo'];
type V3Weekly = components['schemas']['WeeklyExploration'];
type V3WeeklyTrack = components['schemas']['WeeklyTrack'];
type V3Prompt = components['schemas']['ServicePrompt'];
type V3Artwork = components['schemas']['GenreArtwork'];
type V3ArtworkAlbum = components['schemas']['GenreArtworkAlbum'];
type V3Integration = components['schemas']['IntegrationStatus'];

// Transitional bridge: the discover page reads through the v3 hooks while the
// presentational shelves below it still take the v1 page shapes. Every
// default below covers type-level optionality only (the backend sends the
// same payloads the v2 client rendered); it shrinks as shelves migrate to
// the generated contract and dies with the last v1 prop.
const SECTION_TYPES = ['artists', 'albums', 'tracks', 'genres'] as const;

type SectionType = (typeof SECTION_TYPES)[number];

function sectionTypeOf(raw: string): SectionType {
	return (SECTION_TYPES as readonly string[]).includes(raw) ? (raw as SectionType) : 'albums';
}

function toArtist(item: V3Artist): HomeArtist {
	return {
		mbid: item.mbid ?? null,
		local_id: item.local_id ?? null,
		name: item.name,
		image_url: item.image_url ?? null,
		listen_count: item.listen_count ?? null,
		in_library: item.in_library ?? false
	};
}

function toAlbum(item: V3Album): HomeAlbum {
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

function toTopPicks(section: V3TopPicks): TopPicksSection {
	return {
		title: section.title,
		items: (section.items ?? []).map((item) => ({
			album: toAlbum(item.album),
			match_pct: item.match_pct,
			reasons: item.reasons ?? [],
			seed_artist: item.seed_artist ?? null
		})),
		source: section.source ?? null,
		personalizing: section.personalizing ?? false
	};
}

function toBecause(entry: V3Because): BecauseYouListenTo {
	return {
		seed_artist: entry.seed_artist,
		seed_artist_mbid: entry.seed_artist_mbid,
		listen_count: entry.listen_count ?? 0,
		section: toHomeSection(entry.section),
		banner_url: entry.banner_url ?? null,
		wide_thumb_url: entry.wide_thumb_url ?? null,
		fanart_url: entry.fanart_url ?? null
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

function toWeekly(section: V3Weekly): WeeklyExplorationSection {
	return {
		title: section.title,
		playlist_date: section.playlist_date,
		tracks: (section.tracks ?? []).map(toWeeklyTrack),
		source_url: section.source_url ?? ''
	};
}

function toPrompt(prompt: V3Prompt): ServicePrompt {
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

function toArtwork(artwork: V3Artwork): GenreArtwork {
	return {
		kind: artwork.kind === 'gradient' ? 'gradient' : 'collage',
		albums: (artwork.albums ?? []).map(toArtworkAlbum),
		version: artwork.version
	};
}

function toIntegrationStatus(status: V3Integration | null | undefined): Record<string, boolean> {
	if (!status) return {};
	const out: Record<string, boolean> = {};
	for (const [key, value] of Object.entries(status)) {
		if (typeof value === 'boolean') out[key] = value;
	}
	return out;
}

const maybe = <T, U>(value: T | null | undefined, map: (v: T) => U): U | null =>
	value === null || value === undefined ? null : map(value);

export function toDiscoverResponseV1(v3: V3Response): DiscoverResponse {
	return {
		because_you_listen_to: (v3.because_you_listen_to ?? []).map(toBecause),
		discover_queue_enabled: v3.discover_queue_enabled ?? true,
		fresh_releases: maybe(v3.fresh_releases, toHomeSection),
		missing_essentials: maybe(v3.missing_essentials, toHomeSection),
		rediscover: maybe(v3.rediscover, toHomeSection),
		artists_you_might_like: maybe(v3.artists_you_might_like, toHomeSection),
		popular_in_your_genres: maybe(v3.popular_in_your_genres, toHomeSection),
		genre_list: maybe(v3.genre_list, toHomeSection),
		globally_trending: maybe(v3.globally_trending, toHomeSection),
		weekly_exploration: maybe(v3.weekly_exploration, toWeekly),
		lastfm_weekly_artist_chart: maybe(v3.lastfm_weekly_artist_chart, toHomeSection),
		lastfm_weekly_album_chart: maybe(v3.lastfm_weekly_album_chart, toHomeSection),
		lastfm_recent_scrobbles: maybe(v3.lastfm_recent_scrobbles, toHomeSection),
		daily_mixes: (v3.daily_mixes ?? []).map(toHomeSection),
		radio_sections: (v3.radio_sections ?? []).map(toHomeSection),
		top_picks: maybe(v3.top_picks, toTopPicks),
		listeners_like_you: maybe(v3.listeners_like_you, toHomeSection),
		anniversaries: maybe(v3.anniversaries, toHomeSection),
		new_from_followed: maybe(v3.new_from_followed, toHomeSection),
		unexplored_genres: maybe(v3.unexplored_genres, toHomeSection),
		generated_at: v3.generated_at ?? null,
		refresh_started_at: v3.refresh_started_at ?? null,
		section_status: v3.section_status ?? {},
		genre_artwork: Object.fromEntries(
			Object.entries(v3.genre_artwork ?? {}).map(([genre, artwork]) => [genre, toArtwork(artwork)])
		),
		// No consumer reads the version and the backend sends v2.
		genre_artwork_schema_version: 'v2',
		integration_status: toIntegrationStatus(v3.integration_status),
		service_prompts: (v3.service_prompts ?? []).map(toPrompt),
		refreshing: v3.refreshing ?? false,
		service_status: v3.service_status ?? null
	};
}
