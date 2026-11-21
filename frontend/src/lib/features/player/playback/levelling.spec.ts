import { describe, expect, it } from 'vitest';

import type { TrackLoudness } from '../queries/loudness';
import { inAlbumOrder, levelFactor, levelFor } from './levelling';
import type { QueueItem } from './types';

const dB = (factor: number) => Math.round(20 * Math.log10(factor) * 100) / 100;

function loudness(values: Partial<TrackLoudness>): TrackLoudness {
	return {
		track_gain_db: null,
		track_peak: null,
		album_gain_db: null,
		album_peak: null,
		...values
	};
}

function item(
	albumId: string,
	trackNumber: number,
	sourceType: QueueItem['sourceType'] = 'local'
): QueueItem {
	return {
		trackSourceId: `${albumId}-${trackNumber}`,
		trackName: 'Song',
		artistName: 'Artist',
		trackNumber,
		albumId,
		albumName: albumId,
		coverUrl: null,
		sourceType,
		format: 'flac',
		duration: 200
	};
}

describe('levelling', () => {
	it('aims 4 dB above the ReplayGain reference and never past -1 dBFS at the peak', () => {
		// A loud master: -6 dB to the reference plays 2 dB quieter.
		expect(dB(levelFactor(loudness({ track_gain_db: -6, track_peak: 0.5 }), false))).toBe(-2);
		// A quiet one would go up 8 dB, held so its peak reaches -1 dBFS.
		expect(dB(levelFactor(loudness({ track_gain_db: 4, track_peak: 0.5 }), false))).toBe(5.02);
	});

	it('never boosts a song whose peak is not known', () => {
		expect(levelFactor(loudness({ track_gain_db: 4 }), false)).toBe(1);
		expect(dB(levelFactor(loudness({ track_gain_db: -9 }), false))).toBe(-5);
	});

	it('holds the level to -24..+12 dB', () => {
		expect(dB(levelFactor(loudness({ track_gain_db: -40, track_peak: 1 }), false))).toBe(-24);
		expect(dB(levelFactor(loudness({ track_gain_db: 30, track_peak: 0.01 }), false))).toBe(12);
	});

	it('plays an album in order at its album level, each pair falling back to the other', () => {
		const both = loudness({
			track_gain_db: -8,
			track_peak: 0.9,
			album_gain_db: -5,
			album_peak: 0.99
		});
		expect(dB(levelFactor(both, true))).toBe(-1);
		expect(dB(levelFactor(both, false))).toBe(-4);
		expect(dB(levelFactor(loudness({ track_gain_db: -8, track_peak: 0.5 }), true))).toBe(-4);
		expect(dB(levelFactor(loudness({ album_gain_db: -8, album_peak: 0.5 }), false))).toBe(-4);
	});

	it('plays unknown and non-local songs as they are', () => {
		expect(levelFactor(null, false)).toBe(1);
		expect(levelFactor(loudness({}), true)).toBe(1);
		const measured = loudness({ track_gain_db: -6, track_peak: 0.5 });
		expect(dB(levelFor(item('lp', 1), measured, false))).toBe(-2);
		expect(levelFor(item('lp', 1, 'jellyfin'), measured, false)).toBe(1);
		expect(levelFor(item('lp', 1, 'youtube'), measured, false)).toBe(1);
	});

	it('counts a song in album order at both ends of a run', () => {
		const [one, two, three] = [item('lp', 1), item('lp', 2), item('lp', 3)];
		const other = item('single', 1);
		// The first of a run: only the next song follows it.
		expect(inAlbumOrder(other, one, two)).toBe(true);
		// The last of a run: it follows the one before it.
		expect(inAlbumOrder(two, three, other)).toBe(true);
		expect(inAlbumOrder(null, one, null)).toBe(false);
		expect(inAlbumOrder(three, other, null)).toBe(false);
		expect(inAlbumOrder(one, item('lp', 3), null)).toBe(false);
	});
});
