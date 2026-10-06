import type { components } from '$lib/api/v3/openapi';
import type {
	CandidateEvidence,
	OperationResponse,
	OperationState,
	TrackEvidence
} from './LibraryOperationsTypes';

type OperationView = components['schemas']['OperationView'];
type CandidateEvidenceView = components['schemas']['OperationCandidateEvidenceView'];
type TrackEvidenceView = components['schemas']['OperationTrackEvidenceView'];
type Classification = TrackEvidence['classification'];

function classification(value: string): Classification {
	return value === 'supported' || value === 'contradictory' ? value : 'unknown';
}

function toTrackEvidence(view: TrackEvidenceView): TrackEvidence {
	return {
		local_track_id: view.local_track_id,
		classification: classification(view.classification),
		evidence_kinds: view.evidence_kinds,
		candidate_track_title: view.candidate_track_title ?? null,
		candidate_disc_number: view.candidate_disc_number ?? null,
		candidate_track_position: view.candidate_track_position ?? null,
		recording_mbid: view.recording_mbid ?? null,
		release_track_mbid: view.release_track_mbid ?? null
	};
}

function toCandidateEvidence(view: CandidateEvidenceView): CandidateEvidence {
	return {
		release_group_mbid: view.release_group_mbid,
		release_mbid: view.release_mbid ?? null,
		album_title: view.album_title,
		album_artist_name: view.album_artist_name,
		artist_mbid: view.artist_mbid ?? null,
		release_type: view.release_type ?? null,
		release_date: view.release_date ?? null,
		local_album_title: view.local_album_title,
		local_album_artist_name: view.local_album_artist_name,
		album_title_classification: classification(view.album_title_classification),
		album_artist_classification: classification(view.album_artist_classification),
		track_evidence: view.track_evidence.map(toTrackEvidence),
		unmatched_expected_tracks: view.unmatched_expected_tracks,
		score: view.score,
		margin: view.margin,
		reason_code: view.reason_code,
		matcher_version: view.matcher_version
	};
}

// The v3 operation job in the shape the operation panels read. Identity
// repair summaries are not served by this route, so they read as absent.
export function toOperationResponse(view: OperationView): OperationResponse {
	return {
		id: view.id,
		kind: view.kind,
		state: view.state as OperationState,
		expected_work_count: view.expected_work_count,
		completed_count: view.completed_count,
		succeeded_count: view.succeeded_count,
		failed_count: view.failed_count,
		skipped_count: view.skipped_count,
		control_request: view.control_request,
		terminal_code: view.terminal_code ?? null,
		row_revision: view.row_revision,
		event_revision: view.event_revision,
		created_at: view.created_at,
		updated_at: view.updated_at,
		results: view.results.map((result) => ({
			ordinal: result.ordinal,
			action: result.action,
			state: result.state,
			local_album_id: result.local_album_id ?? null,
			local_track_id: result.local_track_id ?? null,
			failure_code: result.failure_code ?? null,
			result: result.result as Record<string, unknown>
		})),
		results_truncated: view.results_truncated,
		repair_summary: null,
		reidentification_candidates: view.reidentification_candidates.map((candidate) => ({
			candidate_key: candidate.candidate_key,
			evidence_revision: candidate.evidence_revision,
			evidence: toCandidateEvidence(candidate.evidence),
			automatic_safe: candidate.automatic_safe
		})),
		selected_reidentification_candidate_key: view.selected_reidentification_candidate_key ?? null
	};
}
