import { describe, expect, it } from 'vitest';
import type { EditionPlan } from '$lib/features/library/queries/EditionQueries.svelte';
import type { LocalTrackInfo } from '$lib/features/album/queries/types';
import { placeOnEdition } from './albumEdition';
import { buildSortedTrackMap, resolveSourceTrack } from './albumTrackResolvers';

type PlanTrack = EditionPlan['tracks'][number];
type PlanFile = EditionPlan['files'][number];

const planTrack = (disc: number, position: number, fileId: string): PlanTrack => ({
	disc_number: disc,
	position,
	local_track_id: fileId,
	title: `Track ${fileId}`,
	recording_mbid: `rec-${fileId}`,
	release_track_mbid: `rt-${fileId}`
});

const planFile = (
	fileId: string,
	fate: PlanFile['fate'],
	trackNumber: number,
	edition: { disc: number; position: number } | null
): PlanFile => ({
	local_track_id: fileId,
	fate,
	disc_number: 1,
	track_number: trackNumber,
	edition_disc_number: edition?.disc ?? null,
	edition_position: edition?.position ?? null,
	match_written: false,
	matched_by_hand: false,
	title: `Track ${fileId}`
});

const kept = Array.from({ length: 23 }, (_, i) => i + 1);
const onEdition = (n: number) =>
	n <= 13 ? { disc: 1, position: n } : { disc: 2, position: n - 13 };

// A two-disc edition (13 + 10) for files tagged as one disc, 1-23.
const plan: Pick<EditionPlan, 'tracks' | 'files'> = {
	tracks: kept.map((n) => planTrack(onEdition(n).disc, onEdition(n).position, `f${n}`)),
	files: [
		...kept.map((n) => planFile(`f${n}`, 'keep', n, onEdition(n))),
		planFile('f24', 'extra', 24, null),
		planFile('f25', 'duplicate', 5, { disc: 1, position: 5 })
	]
};

const localFile = (id: string, trackNumber: number, size: number): LocalTrackInfo => ({
	track_file_id: id,
	title: `Track ${id}`,
	track_number: trackNumber,
	disc_number: 1,
	size_bytes: size,
	format: 'mp3'
});

const local: LocalTrackInfo[] = [
	...kept.map((n) => localFile(`f${n}`, n, 1000 + n)),
	localFile('f24', 24, 1024),
	localFile('f25', 5, 1025)
];

describe('placeOnEdition', () => {
	it('puts files tagged as one disc on the rows the plan places them on, and gives an unplaced file no row', () => {
		const placed = placeOnEdition(plan, local);
		const map = buildSortedTrackMap(placed);
		const row = (disc: number, position: number, index: number) =>
			resolveSourceTrack(disc, position, index, map, placed);

		expect(row(2, 1, 13)?.track_file_id).toBe('f14');
		expect(row(2, 1, 13)?.size_bytes).toBe(1014);
		expect(row(1, 5, 4)?.track_file_id).toBe('f5');
		const ids = placed.map((t) => t.track_file_id);
		expect(ids).not.toContain('f24');
		expect(ids).not.toContain('f25');
	});
});
