import { describe, expect, it } from 'vitest';
import type { DailyMix, MixTrack } from '../queries/types';
import { mixQueue } from './mixPlay';

function track(title: string, kind: MixTrack['kind']): MixTrack {
	const library = kind !== 'new';
	return {
		kind,
		title,
		artist_name: `By ${title}`,
		local_track_id: library ? `t-${title}` : undefined,
		recording_mbid: library ? undefined : `rec-${title}`,
		album: {
			title: `Album ${title}`,
			musicbrainz_id: `rg-${title}`,
			in_library: library,
			requested: false
		}
	};
}

const MIX: DailyMix = {
	id: 'u1:0',
	name: 'A and more',
	artists: [],
	day: '2026-11-28',
	yours: 2,
	new: 1,
	tracks: [track('One', 'artist'), track('Fresh', 'new'), track('Heard', 'sound')]
};

describe('mixQueue', () => {
	it('queues every track in the stored order, a new one through YouTube', () => {
		const queue = mixQueue(MIX, true);
		expect(queue.map((item) => [item.trackName, item.sourceType])).toEqual([
			['One', 'local'],
			['Fresh', 'youtube'],
			['Heard', 'local']
		]);
		expect(queue[1].recordingIds).toEqual({ youtube: 'rec-Fresh' });
		expect(queue[1].trackSourceId).toBe('');
		expect(queue.every((item) => item.playedFrom?.kind === 'mix')).toBe(true);
	});

	it('leaves a new track out while YouTube is not set up', () => {
		expect(mixQueue(MIX, false).map((item) => item.trackName)).toEqual(['One', 'Heard']);
	});
});
