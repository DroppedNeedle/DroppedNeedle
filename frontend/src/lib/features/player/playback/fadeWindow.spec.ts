import { describe, expect, it } from 'vitest';
import { earliestStart, edgesOf, fadeWindow, type SoundEdges } from './fadeWindow';

const edges = (fadeStart: number, soundEnd: number): SoundEdges => ({ fadeStart, soundEnd });

describe('fadeWindow', () => {
	// A 200 s song before a 200 s song, with a 6 s crossfade unless a case says otherwise.
	it.each([
		['no edges: the last 6 s', 180, 6, 200, null, { at: 194, fade: 6 }],
		['no edges, a third of a short song', 180, 6, 12, null, { at: 196, fade: 4 }],
		['no edges, too little left: none', 198, 6, 200, null, null],
		['crossfade off: none', 180, 0, 200, null, null],
		['where the song fades out', 180, 6, 200, edges(185, 190), { at: 185, fade: 5 }],
		['the setting before the sound ends', 180, 6, 200, edges(170, 190), { at: 184, fade: 6 }],
		['a third of a short song', 180, 6, 12, edges(170, 190), { at: 186, fade: 4 }],
		['a hard stop at the sound end', 180, 6, 200, edges(190, 190), { at: 190, fade: 0 }],
		['a fade too short: a hard stop', 180, 6, 200, edges(188, 190), { at: 190, fade: 0 }],
		['a seek into the fade: what is left', 186, 6, 200, edges(185, 190), { at: 186, fade: 4 }],
		['a seek late in it: a hard stop', 188, 6, 200, edges(185, 190), { at: 190, fade: 0 }],
		['a seek past the sound: at once', 195, 6, 200, edges(185, 190), { at: 195, fade: 0 }],
		['a sound end past the length', 180, 6, 200, edges(196, 210), { at: 196, fade: 4 }]
	])('%s', (_, position, setting, shorter, songEdges, expected) => {
		expect(fadeWindow({ position, length: 200, setting, shorter, edges: songEdges })).toEqual(
			expected
		);
	});

	it('loads the next song ahead of the earliest start', () => {
		expect(earliestStart(200, 6, null)).toBe(194);
		expect(earliestStart(200, 6, edges(110, 120))).toBe(114);
		expect(earliestStart(200, 0, edges(110, 120))).toBe(200);
	});

	it('reads edges only when the server gave all three', () => {
		const answer = {
			sound_start_ms: 500,
			fade_start_ms: 185_000,
			sound_end_ms: 190_000
		};
		expect(edgesOf(answer)).toEqual(edges(185, 190));
		expect(edgesOf({ ...answer, sound_start_ms: null })).toBeNull();
		expect(edgesOf({})).toBeNull();
		expect(edgesOf(null)).toBeNull();
	});
});
