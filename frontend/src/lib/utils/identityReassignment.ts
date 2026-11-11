import type { CandidateEvidence, TrackEvidence } from '$lib/queries/library/LibraryOperationsTypes';

/**
 * Conflicts that come only from the identifiers embedded in a file: its recording or
 * release-track MBID belongs to another edition of the same release group. They say
 * nothing about the audio, unlike a duration conflict or an ambiguous pairing.
 */
const ID_ONLY_CONFLICT_KINDS = new Set(['recording_mbid_conflict', 'release_track_mbid_conflict']);

function isReassignable(item: TrackEvidence): boolean {
	return (
		item.classification === 'contradictory' &&
		item.evidence_kinds.length > 0 &&
		item.evidence_kinds.every((kind) => ID_ONLY_CONFLICT_KINDS.has(kind)) &&
		item.candidate_track_title !== null &&
		item.candidate_track_position !== null &&
		!!item.recording_mbid &&
		!!item.release_track_mbid
	);
}

/**
 * Tracks that can be assigned to this edition even though their embedded identifiers
 * disagree, or [] when the candidate is not eligible. Mirrors the server rule in
 * `models.identification.track_reassignment_plan`, which stays authoritative: the title
 * and artist are not contradicted, every local track is supported or an identifier-only
 * conflict, no two tracks claim the same edition track, and the expected tracks left
 * unmatched are exactly the ones the reassigned tracks fill.
 */
export function reassignableTracks(
	evidence: CandidateEvidence,
	indexedTrackCount: number
): TrackEvidence[] {
	if (
		evidence.album_title_classification === 'contradictory' ||
		evidence.album_artist_classification === 'contradictory' ||
		evidence.track_evidence.length !== indexedTrackCount
	) {
		return [];
	}
	const reassigned: TrackEvidence[] = [];
	for (const item of evidence.track_evidence) {
		if (item.classification === 'supported') continue;
		if (!isReassignable(item)) return [];
		reassigned.push(item);
	}
	if (reassigned.length === 0) return [];
	const slots = evidence.track_evidence
		.filter((item) => item.candidate_track_position !== null)
		.map((item) => `${item.candidate_disc_number}:${item.candidate_track_position}`);
	if (new Set(slots).size !== slots.length) return [];
	const filled = reassigned
		.map(
			(item) =>
				`${item.candidate_disc_number}:${item.candidate_track_position}:${item.candidate_track_title}`
		)
		.sort();
	const unmatched = [...evidence.unmatched_expected_tracks].sort();
	if (
		filled.length !== unmatched.length ||
		filled.some((value, index) => value !== unmatched[index])
	) {
		return [];
	}
	return reassigned;
}
