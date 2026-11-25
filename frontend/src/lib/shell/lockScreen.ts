// The lock screen's one media session. While a Discover sample sounds it
// shows the sample and its buttons move through the clips; otherwise it
// shows the player.
import {
	startMediaSession,
	type MediaSessionPlayer
} from '$lib/features/player/playback/mediaSession.svelte';
import { deckSampler } from '$lib/features/discover/state/deckSampler.svelte';

/** The player, or the sampler while it has something on. */
export function lockScreenOf(player: MediaSessionPlayer): MediaSessionPlayer {
	const sample = () => deckSampler.lockScreen;
	return {
		get nowPlaying() {
			return sample()?.nowPlaying ?? player.nowPlaying;
		},
		get playbackState() {
			return sample()?.playbackState ?? player.playbackState;
		},
		get progress() {
			return sample()?.progress ?? player.progress;
		},
		get duration() {
			return sample()?.duration ?? player.duration;
		},
		get isSeekable() {
			return sample() ? false : player.isSeekable;
		},
		play: () => (sample() ? deckSampler.resume() : player.play()),
		pause: () => (sample() ? deckSampler.pause() : player.pause()),
		nextTrack: () => (sample() ? deckSampler.nextClip() : player.nextTrack()),
		previousTrack: () => (sample() ? deckSampler.previousClip() : player.previousTrack()),
		seekTo: (seconds) => {
			if (!sample()) player.seekTo(seconds);
		}
	};
}

/** Starts the lock screen over `player`; returns its stop. */
export function startLockScreen(player: MediaSessionPlayer): () => void {
	return startMediaSession(lockScreenOf(player));
}
