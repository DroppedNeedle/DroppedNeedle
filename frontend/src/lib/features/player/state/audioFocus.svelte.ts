/**
 * One-sound rule: the app must never play two audio streams at once.
 *
 * Lightweight audio surfaces (deck samples, deck videos, previews) claim focus
 * before making sound; claiming pauses the global player (no auto-resume - staying
 * paused is less surprising than a song bursting back in) and stops whichever other
 * lightweight surface held focus. The global player never claims focus here;
 * surfaces watch `playerStore.isPlaying` themselves and release when it starts.
 */
import { playerStore } from '$lib/features/player/state/player.svelte';

interface FocusHolder {
	id: string;
	stop: () => void;
}

let current: FocusHolder | null = null;

export const audioFocus = {
	/** Claim audio focus. Pauses the global player and stops any other holder. */
	claim(id: string, stop: () => void): void {
		if (playerStore.isPlaying) {
			playerStore.pause();
		}
		if (current && current.id !== id) {
			try {
				current.stop();
			} catch {
				// a dead holder must never block a new one
			}
		}
		current = { id, stop };
	},

	/** Release focus (only if still held by `id`). Does not resume anything. */
	release(id: string): void {
		if (current?.id === id) {
			current = null;
		}
	},

	/** Stop and clear whoever holds focus (used when global playback starts). */
	interrupt(): void {
		if (current) {
			try {
				current.stop();
			} catch {
				// ignore: holder already torn down
			}
			current = null;
		}
	},

	get holder(): string | null {
		return current?.id ?? null;
	}
};
