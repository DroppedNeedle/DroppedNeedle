import { CACHE_KEYS, CACHE_TTL } from '$lib/constants';
import type {
	AlbumBasicInfo,
	AlbumTracksInfo,
	LastFmAlbumEnrichment,
	MoreByArtistResponse,
	SimilarAlbumsResponse,
	YouTubeLink,
	YouTubeTrackLink,
	JellyfinAlbumMatch,
	LocalAlbumMatch,
	NavidromeAlbumMatch,
	PlexAlbumMatch
} from '$lib/types';
import { musicBrainzSourceKey } from '$lib/queries/musicbrainz/sourceScope.svelte';
import { clearLocalStorageNamespace, createLocalStorageCache } from '$lib/utils/localStorageCache';

const MAX_ALBUM_DETAIL_CACHE_ENTRIES = 120;

function musicBrainzCacheNamespace(): string {
	const scope = musicBrainzSourceKey();
	return [
		`m-${encodeURIComponent(scope.source_mode)}`,
		`u-${encodeURIComponent(scope.user_id ?? 'anonymous')}`,
		`s-${encodeURIComponent(scope.source_id || 'legacy')}`,
		`g-${scope.generation}`
	].join('_');
}

type AlbumDiscoveryCachePayload = {
	moreByArtist: MoreByArtistResponse | null;
	similarAlbums: SimilarAlbumsResponse | null;
};

type AlbumYouTubeCachePayload = {
	albumLink: YouTubeLink | null;
	trackLinks: YouTubeTrackLink[];
};

type AlbumSourceMatchCachePayload = {
	jellyfin: JellyfinAlbumMatch | null;
	local: LocalAlbumMatch | null;
	navidrome: NavidromeAlbumMatch | null;
	plex: PlexAlbumMatch | null;
};

export const albumBasicCache = createLocalStorageCache<AlbumBasicInfo>(
	CACHE_KEYS.ALBUM_BASIC_CACHE,
	CACHE_TTL.ALBUM_DETAIL_BASIC,
	{ maxEntries: MAX_ALBUM_DETAIL_CACHE_ENTRIES, keyNamespace: musicBrainzCacheNamespace }
);

export const albumTracksCache = createLocalStorageCache<AlbumTracksInfo>(
	CACHE_KEYS.ALBUM_TRACKS_CACHE,
	CACHE_TTL.ALBUM_DETAIL_TRACKS,
	{ maxEntries: MAX_ALBUM_DETAIL_CACHE_ENTRIES, keyNamespace: musicBrainzCacheNamespace }
);

export const albumDiscoveryCache = createLocalStorageCache<AlbumDiscoveryCachePayload>(
	CACHE_KEYS.ALBUM_DISCOVERY_CACHE,
	CACHE_TTL.ALBUM_DETAIL_DISCOVERY,
	{ maxEntries: MAX_ALBUM_DETAIL_CACHE_ENTRIES, keyNamespace: musicBrainzCacheNamespace }
);

export const albumLastFmCache = createLocalStorageCache<LastFmAlbumEnrichment>(
	CACHE_KEYS.ALBUM_LASTFM_CACHE,
	CACHE_TTL.ALBUM_DETAIL_LASTFM,
	{ maxEntries: MAX_ALBUM_DETAIL_CACHE_ENTRIES }
);

export const albumYouTubeCache = createLocalStorageCache<AlbumYouTubeCachePayload>(
	CACHE_KEYS.ALBUM_YOUTUBE_CACHE,
	CACHE_TTL.ALBUM_DETAIL_YOUTUBE,
	{ maxEntries: MAX_ALBUM_DETAIL_CACHE_ENTRIES }
);

export const albumSourceMatchCache = createLocalStorageCache<AlbumSourceMatchCachePayload>(
	CACHE_KEYS.ALBUM_SOURCE_MATCH_CACHE,
	CACHE_TTL.ALBUM_DETAIL_SOURCE_MATCH,
	{ maxEntries: MAX_ALBUM_DETAIL_CACHE_ENTRIES }
);

// MusicBrainz source changes can make these provider-backed album payloads stale. Keep
// Last.fm/YouTube/integration and mixed local-search namespaces untouched: they are
// separate providers or can contain local/user data.
const MUSICBRAINZ_PROVIDER_CACHE_NAMESPACES = [
	CACHE_KEYS.ALBUM_BASIC_CACHE,
	CACHE_KEYS.ALBUM_TRACKS_CACHE,
	CACHE_KEYS.ALBUM_DISCOVERY_CACHE
] as const;

export function clearMusicBrainzProviderCaches(): boolean {
	let allCleared = true;
	for (const namespace of MUSICBRAINZ_PROVIDER_CACHE_NAMESPACES) {
		try {
			clearLocalStorageNamespace(namespace);
		} catch {
			allCleared = false;
		}
	}
	return allCleared;
}
