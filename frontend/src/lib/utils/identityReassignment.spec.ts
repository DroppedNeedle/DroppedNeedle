import { describe, expect, it } from 'vitest';
import type { CandidateEvidence, TrackEvidence } from '$lib/queries/library/LibraryOperationsTypes';
import { reassignableTracks } from './identityReassignment';

function supported(index: number): TrackEvidence {
	return {
		local_track_id: `t${index}`,
		classification: 'supported',
		evidence_kinds: [],
		candidate_track_title: `Track ${index}`,
		candidate_disc_number: 1,
		candidate_track_position: index,
		recording_mbid: `rec-${index}`,
		release_track_mbid: `rt-${index}`
	};
}

function conflicting(index: number, kinds = ['release_track_mbid_conflict']): TrackEvidence {
	return { ...supported(index), classification: 'contradictory', evidence_kinds: kinds };
}

function evidence(tracks: TrackEvidence[], missing: string[]): CandidateEvidence {
	return {
		release_group_mbid: 'rg',
		release_mbid: 'release',
		album_title: 'Album',
		album_artist_name: 'Artist',
		artist_mbid: null,
		release_type: null,
		release_date: null,
		local_album_title: 'Album',
		local_album_artist_name: 'Artist',
		album_title_classification: 'supported',
		album_artist_classification: 'supported',
		track_evidence: tracks,
		unmatched_expected_tracks: missing,
		score: 1,
		margin: 0,
		reason_code: 'CONFLICTING_TRACK_EVIDENCE',
		matcher_version: 'test'
	};
}

describe('reassignableTracks', () => {
	it('offers identifier-only conflicts that fill the unmatched edition slots', () => {
		const result = reassignableTracks(
			evidence([supported(1), supported(2), conflicting(3)], ['1:3:Track 3']),
			3
		);
		expect(result.map((item) => item.local_track_id)).toEqual(['t3']);
	});

	it('refuses a real conflict such as a duration mismatch', () => {
		expect(
			reassignableTracks(
				evidence([supported(1), conflicting(2, ['duration_conflict'])], ['1:2:Track 2']),
				2
			)
		).toEqual([]);
	});

	it('never hides a genuinely missing track behind a reassignment', () => {
		expect(
			reassignableTracks(
				evidence([supported(1), conflicting(2)], ['1:2:Track 2', '1:3:Track 3']),
				2
			)
		).toEqual([]);
	});

	it('refuses when the album title or artist is contradicted', () => {
		const candidate = evidence([supported(1), conflicting(2)], ['1:2:Track 2']);
		candidate.album_title_classification = 'contradictory';
		expect(reassignableTracks(candidate, 2)).toEqual([]);
	});

	it('refuses when every local track is already supported', () => {
		expect(reassignableTracks(evidence([supported(1), supported(2)], []), 2)).toEqual([]);
	});
});
