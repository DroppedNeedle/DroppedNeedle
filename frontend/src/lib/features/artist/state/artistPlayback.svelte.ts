import { PersistedState } from 'runed';
import { useIntegrationStatus } from '$lib/features/home/queries/home.svelte';
import { playerStore } from '$lib/features/player/state/player.svelte';
import { openGlobalPlaylistModal } from '$lib/features/playlists/state/playlistModal.svelte';
import type { SourceType } from '$lib/features/player/playback/types';
import type { ReleaseGroup } from '$lib/features/album/queries/types';
import { toastStore } from '$lib/stores/toast';
import { createArtistTrackLoader } from './artistTrackLoader.svelte';

export type PlaybackSourceType = Exclude<SourceType, 'youtube'>;

const PLAYBACK_SOURCES: { key: PlaybackSourceType; integrationKey: string }[] = [
	{ key: 'local', integrationKey: 'localfiles' },
	{ key: 'navidrome', integrationKey: 'navidrome' },
	{ key: 'jellyfin', integrationKey: 'jellyfin' },
	{ key: 'plex', integrationKey: 'plex' }
];

/**
 * Which playback sources are on and which one this artist plays from: the
 * one last chosen for the artist, else the one last used anywhere, else the
 * first that is on. A choice is remembered per artist.
 */
export function createPlaybackSource(getArtistId: () => string) {
	const integrations = useIntegrationStatus();
	const perArtist = new PersistedState<Record<string, PlaybackSourceType>>(
		'artist-playback-sources',
		{}
	);
	const lastUsed = new PersistedState<PlaybackSourceType | null>('last-playback-source', null);

	const available = $derived.by(() => {
		const status = integrations.current;
		return PLAYBACK_SOURCES.filter(
			(source) => status[source.integrationKey as keyof typeof status] === true
		).map((source) => source.key);
	});

	const selected = $derived.by<PlaybackSourceType>(() => {
		const chosen = perArtist.current[getArtistId()];
		if (chosen && available.includes(chosen)) return chosen;
		const last = lastUsed.current;
		if (last && available.includes(last)) return last;
		return available[0] ?? 'local';
	});

	return {
		get available() {
			return available;
		},
		get selected() {
			return selected;
		},
		choose(source: PlaybackSourceType) {
			perArtist.current = { ...perArtist.current, [getArtistId()]: source };
			lastUsed.current = source;
		}
	};
}

export interface ReleasePlaybackConfig {
	artistName: string;
	artistId: string;
	/** The releases to play; only those in the library are played. */
	releases: ReleaseGroup[];
	source: PlaybackSourceType;
}

/**
 * Plays, shuffles, queues and lists an artist's owned releases from one
 * source, through the player, with toasts for what it did.
 */
export function createReleasePlayback(getConfig: () => ReleasePlaybackConfig) {
	const loader = createArtistTrackLoader(
		getConfig,
		(items, startIndex, shuffle) => playerStore.playQueue(items, startIndex, shuffle),
		(items) => playerStore.appendQueueSilent(items),
		() => playerStore.regenerateShuffleOrder(),
		(message, type) => toastStore.show({ message, type: type === 'error' ? 'error' : 'info' })
	);

	async function addToQueue() {
		const tracks = await loader.fetchTracks();
		if (tracks.length === 0) return;
		playerStore.addMultipleToQueue(tracks);
		toastStore.show({
			message: `Added ${tracks.length} tracks to queue from ${getConfig().artistName}`,
			type: 'success'
		});
	}

	async function addToPlaylist() {
		const tracks = await loader.fetchTracks();
		if (tracks.length > 0) openGlobalPlaylistModal(tracks);
	}

	return {
		get loading() {
			return loader.loading;
		},
		get progressText() {
			return loader.progressText;
		},
		play: () => loader.playAll(),
		shuffle: () => loader.shuffleAll(),
		addToQueue,
		addToPlaylist,
		abort: () => loader.abort()
	};
}
