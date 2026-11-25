/**
 * When the next song starts, and how long the two blend, for a crossfade.
 * Without a song's sound edges, or with smart fades off, the fade is the
 * crossfade setting at the very end of the song. With them, the next song
 * starts where this one fades out, the setting being the longest a fade can
 * be, and a song that stops hard is followed as its sound ends, with no
 * overlap. Pure; the transition times the move from it.
 */
import type { TrackLoudness } from '../queries/loudness';

// A crossfade shorter than this is none: the songs join instead.
export const MIN_FADE_S = 3;

/** Where a song starts to fade out and where its sound ends, in seconds. */
export interface SoundEdges {
	fadeStart: number;
	soundEnd: number;
}

/** When the next song starts, by this song's position, and the fade's length; 0 joins. */
export interface Move {
	at: number;
	fade: number;
}

/** A song's sound edges from its loudness answer, or null when the server has none. */
export function edgesOf(loudness: TrackLoudness | null | undefined): SoundEdges | null {
	const fadeStart = loudness?.fade_start_ms;
	const soundEnd = loudness?.sound_end_ms;
	if (fadeStart == null || soundEnd == null || loudness?.sound_start_ms == null) return null;
	return { fadeStart: fadeStart / 1000, soundEnd: soundEnd / 1000 };
}

export interface FadeAsk {
	/** Where this song is, and its length, in seconds. */
	position: number;
	length: number;
	/** The crossfade setting, in seconds. */
	setting: number;
	/** The shorter of the two songs' lengths: a fade is at most a third of it. */
	shorter: number;
	/** This song's sound edges, null when there are none or smart fades are off. */
	edges: SoundEdges | null;
}

/**
 * The move to the next song, or null for no crossfade. With edges the fade
 * runs from where the song fades out (or the setting before its sound ends,
 * whichever is later) to where its sound ends, and from what is left after
 * a seek into it; one too short is a hard stop at the sound's end.
 */
export function fadeWindow({ position, length, setting, shorter, edges }: FadeAsk): Move | null {
	if (!edges) {
		const fade = Math.min(setting, shorter / 3, length - position);
		return fade >= MIN_FADE_S ? { at: length - fade, fade } : null;
	}
	const end = Math.min(edges.soundEnd, length);
	const from = Math.max(edges.fadeStart, end - setting, end - shorter / 3, position);
	if (end - from >= MIN_FADE_S) return { at: from, fade: end - from };
	return { at: Math.max(end, position), fade: 0 };
}

/**
 * The earliest position the next song can start at with this setting, so
 * it is loaded in time: the setting before the end of the sound, or of the
 * song without edges; the song's end with crossfade off.
 */
export function earliestStart(length: number, setting: number, edges: SoundEdges | null): number {
	if (setting <= 0) return length;
	return Math.min(length, edges?.soundEnd ?? length) - setting;
}
