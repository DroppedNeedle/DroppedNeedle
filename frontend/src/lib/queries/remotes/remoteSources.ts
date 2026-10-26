import type { Component } from 'svelte';

import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
import PlexIcon from '$lib/components/PlexIcon.svelte';
import { launchJellyfinPlayback } from '$lib/player/launchJellyfinPlayback';
import { launchNavidromePlayback } from '$lib/player/launchNavidromePlayback';
import { launchPlexPlayback } from '$lib/player/launchPlexPlayback';
import {
	buildDiscoveryQueueFromJellyfin,
	buildDiscoveryQueueFromNavidrome,
	buildDiscoveryQueueFromPlex,
	buildQueueItemsFromJellyfin,
	buildQueueItemsFromNavidrome,
	buildQueueItemsFromPlex
} from '$lib/player/queueHelpers';
import type { PlaybackMeta, QueueItem } from '$lib/player/types';
import type { JellyfinAlbumSummary, NavidromeAlbumSummary, PlexAlbumSummary } from '$lib/types';

import {
	toJellyfinAlbum,
	toJellyfinTrack,
	toNavidromeAlbum,
	toNavidromeTrack,
	toPlexAlbum,
	toPlexTrack
} from './remoteAdapters';
import type { RemoteAlbum, RemoteSource, RemoteTrack } from './types';

export interface RemoteSortOption {
	value: string;
	label: string;
}

/** A list's sort choices; the first is the default and the only one that starts ascending. */
export type RemoteSorts = RemoteSortOption[];

/**
 * What a media server offers beyond the shared pages. The remote pages read
 * these, never the source name.
 */
export interface RemoteCapabilities {
	/** Listening history on the hub and the activity page. */
	activity: boolean;
	/** A mood filter on the albums page. */
	moods: boolean;
	/** Year, tag and studio filters from the server's facets. */
	facets: boolean;
	/** A fixed decade filter on the albums page. */
	decades: boolean;
	/** The hub's most played artists. */
	playCounts: boolean;
	/** The hub's instant mix from a genre. */
	mix: boolean;
	/** The hub's random tracks, filterable by genre. */
	random: boolean;
	/** The hub's recommended albums. */
	recommendations: boolean;
	/** Albums like the last one played, on the hub. */
	similarAlbums: boolean;
	/** Top songs, similar songs and a biography for a favourite artist, on the hub. */
	artistInfo: boolean;
	/** Starred albums, artists and tracks. */
	favorites: boolean;
	/** The hub's recently added albums. */
	recentlyAdded: boolean;
	/** The genre browser takes several genres at once. */
	multiGenre: boolean;
	/** The music folders a user picks on their profile scope every read. */
	folders: boolean;
}

export interface RemoteSourceDescriptor {
	source: RemoteSource;
	label: string;
	icon: Component<{ class?: string }>;
	/** Text colour in the server's brand. */
	tone: string;
	/** The hairline under the page top, in the server's brand. */
	line: string;
	/** The badge on an album card. */
	badge: string;
	sorts: {
		albums: RemoteSorts;
		/** The albums sort that lists the newest first. */
		albumsAdded: string;
		artists: RemoteSorts | null;
		tracks: RemoteSorts | null;
	};
	/** Where the hub's artist index sends an artist with no MusicBrainz id. */
	artistSearch: 'albums' | 'artists';
	capabilities: RemoteCapabilities;
	/** The track's key in the play queue, or null when it cannot be streamed. */
	playKey: (track: RemoteTrack) => string | null;
	/** Queue items for loose tracks (a list, a mix, a genre). */
	queue: (tracks: RemoteTrack[]) => QueueItem[];
	/** Queue items for an album's tracks, in album order. */
	albumQueue: (tracks: RemoteTrack[], meta: PlaybackMeta) => QueueItem[];
	/** Plays an album's tracks from the start. */
	playAlbum: (tracks: RemoteTrack[], shuffle: boolean, meta: PlaybackMeta) => void;
	/** The album dialog's and the shared shelves' album shape. */
	dialogAlbum: (album: RemoteAlbum) => RemoteDialogAlbum;
	/** The id field of `dialogAlbum`'s shape. */
	idKey: string;
}

export type RemoteDialogAlbum = JellyfinAlbumSummary | NavidromeAlbumSummary | PlexAlbumSummary;

const NONE: RemoteCapabilities = {
	activity: false,
	moods: false,
	facets: false,
	decades: false,
	playCounts: false,
	mix: false,
	random: false,
	recommendations: false,
	similarAlbums: false,
	artistInfo: false,
	favorites: false,
	recentlyAdded: false,
	multiGenre: false,
	folders: false
};

const byTrackNumber = (tracks: RemoteTrack[]) =>
	[...tracks].sort((a, b) => (a.track_number ?? 0) - (b.track_number ?? 0));

export const remoteSources: Record<RemoteSource, RemoteSourceDescriptor> = {
	plex: {
		source: 'plex',
		label: 'Plex',
		icon: PlexIcon,
		tone: 'text-plex',
		line: 'via-plex',
		badge: 'border-0 bg-plex text-black',
		sorts: {
			albums: [
				{ value: 'name', label: 'Name' },
				{ value: 'date_added', label: 'Date Added' },
				{ value: 'year', label: 'Year' },
				{ value: 'play_count', label: 'Play Count' },
				{ value: 'rating', label: 'Rating' },
				{ value: 'last_played', label: 'Last Played' }
			],
			albumsAdded: 'date_added',
			artists: [
				{ value: 'titleSort', label: 'Name' },
				{ value: 'addedAt', label: 'Recently Added' },
				{ value: 'lastViewedAt', label: 'Recently Played' }
			],
			tracks: [
				{ value: 'titleSort', label: 'Title' },
				{ value: 'addedAt', label: 'Recently Added' },
				{ value: 'lastViewedAt', label: 'Recently Played' },
				{ value: 'duration', label: 'Duration' }
			]
		},
		artistSearch: 'artists',
		capabilities: {
			...NONE,
			activity: true,
			moods: true,
			decades: true,
			recommendations: true,
			recentlyAdded: true
		},
		// Plex streams a track's media part, so a track without one cannot play.
		playKey: (track) => track.part_key ?? null,
		queue: (tracks) => buildDiscoveryQueueFromPlex(tracks.map(toPlexTrack)),
		albumQueue: (tracks, meta) =>
			buildQueueItemsFromPlex(byTrackNumber(tracks).map(toPlexTrack), meta),
		playAlbum: (tracks, shuffle, meta) =>
			launchPlexPlayback(byTrackNumber(tracks).map(toPlexTrack), 0, shuffle, meta),
		dialogAlbum: toPlexAlbum,
		idKey: 'plex_id'
	},
	jellyfin: {
		source: 'jellyfin',
		label: 'Jellyfin',
		icon: JellyfinIcon,
		tone: 'text-jellyfin',
		line: 'via-jellyfin',
		badge: 'badge-info',
		sorts: {
			albums: [
				{ value: 'SortName', label: 'Name' },
				{ value: 'DateCreated', label: 'Date Added' },
				{ value: 'ProductionYear', label: 'Year' }
			],
			albumsAdded: 'DateCreated',
			artists: [
				{ value: 'SortName', label: 'Name' },
				{ value: 'DateCreated', label: 'Recently Added' },
				{ value: 'DatePlayed', label: 'Recently Played' },
				{ value: 'PlayCount', label: 'Most Played' }
			],
			tracks: [
				{ value: 'SortName', label: 'Title' },
				{ value: 'DateCreated', label: 'Recently Added' },
				{ value: 'DatePlayed', label: 'Recently Played' },
				{ value: 'PlayCount', label: 'Most Played' },
				{ value: 'Runtime', label: 'Duration' }
			]
		},
		artistSearch: 'artists',
		capabilities: {
			...NONE,
			facets: true,
			playCounts: true,
			mix: true,
			similarAlbums: true,
			favorites: true,
			recentlyAdded: true,
			multiGenre: true
		},
		playKey: (track) => track.id,
		queue: (tracks) => buildDiscoveryQueueFromJellyfin(tracks.map(toJellyfinTrack)),
		albumQueue: (tracks, meta) =>
			buildQueueItemsFromJellyfin(byTrackNumber(tracks).map(toJellyfinTrack), meta),
		playAlbum: (tracks, shuffle, meta) =>
			launchJellyfinPlayback(byTrackNumber(tracks).map(toJellyfinTrack), 0, shuffle, meta),
		dialogAlbum: toJellyfinAlbum,
		idKey: 'jellyfin_id'
	},
	navidrome: {
		source: 'navidrome',
		label: 'Navidrome',
		icon: NavidromeIcon,
		tone: 'text-navidrome',
		line: 'via-navidrome',
		badge: 'badge-primary',
		sorts: {
			albums: [
				{ value: 'name', label: 'Name' },
				{ value: 'date_added', label: 'Date Added' },
				{ value: 'year', label: 'Year' }
			],
			albumsAdded: 'date_added',
			artists: null,
			tracks: null
		},
		artistSearch: 'albums',
		capabilities: {
			...NONE,
			random: true,
			artistInfo: true,
			favorites: true,
			multiGenre: true,
			folders: true
		},
		playKey: (track) => track.id,
		queue: (tracks) => buildDiscoveryQueueFromNavidrome(tracks.map(toNavidromeTrack)),
		albumQueue: (tracks, meta) =>
			buildQueueItemsFromNavidrome(byTrackNumber(tracks).map(toNavidromeTrack), meta),
		playAlbum: (tracks, shuffle, meta) =>
			launchNavidromePlayback(byTrackNumber(tracks).map(toNavidromeTrack), 0, shuffle, meta),
		dialogAlbum: toNavidromeAlbum,
		idKey: 'navidrome_id'
	}
};

export function isRemoteSource(value: string): value is RemoteSource {
	return Object.hasOwn(remoteSources, value);
}

/** The sort order a field starts in: the default field ascending, the rest newest or most first. */
export function defaultSortOrder(sorts: RemoteSorts, field: string): 'asc' | 'desc' {
	return field === sorts[0]?.value ? 'asc' : 'desc';
}
