import type { components } from '$lib/api/v3/openapi';
import type {
	BecauseYouListenTo,
	DiscoverQueueEnrichment,
	DiscoverQueueItemFull,
	DiscoverResponse,
	TopPicksSection
} from '$lib/types';
import {
	maybe,
	toAlbum,
	toGenreArtworkMap,
	toHomeSection,
	toIntegrationStatus,
	toPrompt,
	toWeekly
} from '../chartAdapters';

type V3Response = components['schemas']['DiscoverResponse'];
type V3TopPicks = components['schemas']['TopPicksSection'];
type V3Because = components['schemas']['BecauseYouListenTo'];
type V3QueueItem = components['schemas']['QueueItemFull'];
type V3QueueEnrichment = components['schemas']['QueueEnrichment'];

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
		genre_artwork: toGenreArtworkMap(v3.genre_artwork),
		// No consumer reads the version and the backend sends v2.
		genre_artwork_schema_version: 'v2',
		integration_status: toIntegrationStatus(v3.integration_status),
		service_prompts: (v3.service_prompts ?? []).map(toPrompt),
		refreshing: v3.refreshing ?? false,
		service_status: v3.service_status ?? null
	};
}

export function toQueueEnrichment(enrichment: V3QueueEnrichment): DiscoverQueueEnrichment {
	return {
		artist_mbid: enrichment.artist_mbid ?? null,
		release_date: enrichment.release_date ?? null,
		country: enrichment.country ?? null,
		tags: enrichment.tags ?? [],
		youtube_url: enrichment.youtube_url ?? null,
		youtube_search_url: enrichment.youtube_search_url ?? '',
		youtube_search_available: enrichment.youtube_search_available ?? false,
		artist_description: enrichment.artist_description ?? null,
		listen_count: enrichment.listen_count ?? null
	};
}

// Light and enriched cards share every field but `enrichment`, so one
// adapter reads both.
export function toQueueItem(item: V3QueueItem): DiscoverQueueItemFull {
	return {
		release_group_mbid: item.release_group_mbid,
		album_name: item.album_name,
		artist_name: item.artist_name,
		artist_mbid: item.artist_mbid,
		cover_url: item.cover_url ?? null,
		recommendation_reason: item.recommendation_reason,
		is_wildcard: item.is_wildcard ?? false,
		in_library: item.in_library ?? false,
		...(item.enrichment ? { enrichment: toQueueEnrichment(item.enrichment) } : {})
	};
}
