import { untrack } from 'svelte';
import { coverSrc } from '$lib/api/covers';
import { apiOrigin, getApiUrl } from '$lib/api/api-utils';
import { isValidMbid } from '$lib/utils/formatting';
import type { NowPlaying, PlaybackState } from './types';

/** What the lock screen reads from the player and the actions it calls. */
export interface MediaSessionPlayer {
	readonly nowPlaying: NowPlaying | null;
	readonly playbackState: PlaybackState;
	readonly progress: number;
	readonly duration: number;
	readonly isSeekable: boolean;
	play(): void;
	pause(): void;
	nextTrack(): void;
	previousTrack(): void;
	seekTo(seconds: number): void;
}

// The system moves the position on by itself while playing; it is told again
// this often, and at once when the position jumps further than drift would.
const REFRESH_MS = 5000;
const JUMP_S = 2;
const COVER_SIZES = [250, 500] as const;

// iOS shows either next and previous or 10 s skips, never both, so there are no skips.
const TRANSPORT = ['play', 'pause', 'nexttrack', 'previoustrack'] as const;
const SEEKING = ['seekto'] as const;

/** The cover the player bar shows, as the lock screen's artwork. */
export function artworkFor(np: NowPlaying): MediaImage[] {
	if (isValidMbid(np.albumId)) {
		return COVER_SIZES.map((size) => ({
			src: coverSrc('release-group', np.albumId, size),
			sizes: `${size}x${size}`
		}));
	}
	const url = np.coverUrl;
	if (!url) return [];
	// One of our own routes, or an address the player already loads as its cover:
	// https, or the configured API origin's own http.
	const ours = url.startsWith('/') && !url.startsWith('//');
	const parsed = parsedUrl(ours ? getApiUrl(url) : url);
	if (!parsed) return [];
	const apiAt = parsedUrl(apiOrigin() ?? '')?.origin;
	if (!ours && parsed.protocol !== 'https:' && parsed.origin !== apiAt) return [];
	return [{ src: parsed.href }];
}

/** An address read against the page, or null when it does not parse. */
function parsedUrl(address: string): { href: string; protocol: string; origin: string } | null {
	if (!address) return null;
	try {
		const { href, protocol, origin } = new URL(address, location.href);
		return { href, protocol, origin };
	} catch {
		return null;
	}
}

function sessionStateOf(state: PlaybackState): MediaSessionPlaybackState {
	if (state === 'idle') return 'none';
	return state === 'playing' || state === 'buffering' || state === 'loading' ? 'playing' : 'paused';
}

function finite(n: number): number {
	return Number.isFinite(n) ? n : 0;
}

/**
 * Shows the playing song on the lock screen and in the system's media
 * controls, and hands their buttons to the player's own actions. Returns
 * the stop, which clears all of it.
 */
export function startMediaSession(
	player: MediaSessionPlayer,
	session: MediaSession | undefined = browserMediaSession(),
	now: () => number = () => performance.now()
): () => void {
	if (!session) return () => {};
	const media = session;

	function handle(action: MediaSessionAction, handler: MediaSessionActionHandler | null): void {
		try {
			media.setActionHandler(action, handler);
		} catch {
			// A browser that does not know the action leaves it off its controls.
		}
	}

	function position(state: MediaPositionState | null): void {
		try {
			if (state) media.setPositionState(state);
			else media.setPositionState();
		} catch {
			// Refused values leave the last position shown.
		}
	}

	const handlers: Record<
		(typeof TRANSPORT)[number] | (typeof SEEKING)[number],
		MediaSessionActionHandler
	> = {
		play: () => player.play(),
		pause: () => player.pause(),
		nexttrack: () => player.nextTrack(),
		previoustrack: () => player.previousTrack(),
		seekto: (details) => {
			if (details.seekTime === undefined || !Number.isFinite(details.seekTime)) return;
			player.seekTo(Math.max(0, details.seekTime));
		}
	};

	let metadataKey: string | null = null;
	let told: {
		at: number;
		position: number;
		duration: number;
		playing: boolean;
		track: NowPlaying | null;
	} | null = null;

	function clear(): void {
		metadataKey = null;
		told = null;
		media.metadata = null;
		media.playbackState = 'none';
		position(null);
		for (const action of [...TRANSPORT, ...SEEKING]) handle(action, null);
	}

	const stopEffects = $effect.root(() => {
		// The song: title, artist, album and cover.
		$effect(() => {
			const np = player.nowPlaying;
			untrack(() => {
				if (!np) {
					if (metadataKey !== null) clear();
					return;
				}
				const artwork = artworkFor(np);
				const title = np.trackName || np.albumName;
				const key = JSON.stringify([title, np.artistName, np.albumName, artwork[0]?.src]);
				if (key === metadataKey) return;
				metadataKey = key;
				try {
					media.metadata = new MediaMetadata({
						title,
						artist: np.artistName,
						album: np.albumName,
						artwork
					});
				} catch {
					// Artwork the browser refuses leaves the song shown without a cover.
					media.metadata = new MediaMetadata({ title, artist: np.artistName, album: np.albumName });
				}
			});
		});

		// Its buttons, while a song is loaded; seeking only where the song can seek.
		$effect(() => {
			const loaded = player.nowPlaying !== null;
			const seekable = player.isSeekable;
			untrack(() => {
				for (const action of TRANSPORT) handle(action, loaded ? handlers[action] : null);
				for (const action of SEEKING) {
					handle(action, loaded && seekable ? handlers[action] : null);
				}
			});
		});

		$effect(() => {
			const state = player.playbackState;
			if (player.nowPlaying) media.playbackState = sessionStateOf(state);
		});

		// Where in the song, told on a change and every few seconds, not every frame.
		$effect(() => {
			const track = player.nowPlaying;
			const playing = player.playbackState === 'playing';
			const at = finite(player.progress);
			const length = finite(player.duration);
			untrack(() => {
				if (!track) return;
				if (length <= 0) {
					if (told) position(null);
					told = null;
					return;
				}
				const time = now();
				const pos = Math.min(Math.max(0, at), length);
				if (told && told.track === track && told.playing === playing && told.duration === length) {
					const expected = told.position + (playing ? (time - told.at) / 1000 : 0);
					if (Math.abs(pos - expected) <= JUMP_S && time - told.at < REFRESH_MS) return;
				}
				told = { at: time, position: pos, duration: length, playing, track };
				position({ duration: length, playbackRate: 1, position: pos });
			});
		});
	});

	return () => {
		stopEffects();
		clear();
	};
}

function browserMediaSession(): MediaSession | undefined {
	if (typeof navigator === 'undefined' || !('mediaSession' in navigator)) return undefined;
	if (typeof MediaMetadata === 'undefined') return undefined;
	return navigator.mediaSession;
}
