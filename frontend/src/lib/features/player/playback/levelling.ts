/**
 * "Even out volume": the level one song plays at, from the ReplayGain 2.0
 * numbers the server gives (measured, else the file's tags). Pure; the
 * player sets the result on the song's deck before it plays.
 */
import type { TrackLoudness } from '../queries/loudness';
import { followsOnAlbum } from './albumOrder';
import type { QueueItem } from './types';

// ReplayGain aims at -18 LUFS; this aims at -14, so a typical loud master
// plays a few dB quieter rather than most songs a lot louder.
const TARGET_ABOVE_REFERENCE_DB = 4;
const LOWEST_DB = -24;
const HIGHEST_DB = 12;
// The stored peak is a sample peak; the true peak can sit about 1 dB above it.
const CEILING_DBFS = -1;

/**
 * Whether `item` plays in its album's order: it follows the song played
 * before it, or the next song follows it. Judged once, when its level is set.
 */
export function inAlbumOrder(
	previous: QueueItem | null | undefined,
	item: QueueItem,
	next: QueueItem | null | undefined
): boolean {
	return (
		(previous != null && followsOnAlbum(previous, item)) ||
		(next != null && followsOnAlbum(item, next))
	);
}

/**
 * The linear gain a song plays at: its album numbers in album order, else
 * its track numbers, each falling back to the other, 4 dB above the
 * ReplayGain reference, held to -24..+12 dB and never past -1 dBFS at its
 * peak. With no peak known it is never raised. With no gain known, 1.
 */
export function levelFactor(loudness: TrackLoudness | null, albumMode: boolean): number {
	if (!loudness) return 1;
	const track = { gain: loudness.track_gain_db, peak: loudness.track_peak };
	const album = { gain: loudness.album_gain_db, peak: loudness.album_peak };
	const [first, second] = albumMode ? [album, track] : [track, album];
	const pair = first.gain != null ? first : second.gain != null ? second : null;
	if (!pair || pair.gain == null) return 1;
	let level = Math.min(HIGHEST_DB, Math.max(LOWEST_DB, pair.gain + TARGET_ABOVE_REFERENCE_DB));
	const peak = pair.peak ?? 0;
	level = peak > 0 ? Math.min(level, CEILING_DBFS - 20 * Math.log10(peak)) : Math.min(level, 0);
	return 10 ** (level / 20);
}

/** `levelFactor` for a queue item: only the library's own songs are levelled. */
export function levelFor(
	item: QueueItem,
	loudness: TrackLoudness | null,
	albumMode: boolean
): number {
	return item.sourceType === 'local' ? levelFactor(loudness, albumMode) : 1;
}
