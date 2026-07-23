export type LibraryWorkState =
	| 'queued'
	| 'discovering'
	| 'indexing'
	| 'reconciling'
	| 'pausing'
	| 'paused'
	| 'stopping'
	| 'completed'
	| 'cancelled'
	| 'superseded_policy_changed'
	| 'failed'
	| 'running'
	| 'idle';

export interface LibraryActivityItem {
	kind: 'scan' | 'identification';
	state: LibraryWorkState;
	label: string;
	processed: number;
	total: number | null;
	indeterminate: boolean;
	updated_at: number;
	started_at: number | null;
	waiting_count: number;
	identified_count: number;
	kept_local_count: number;
	needs_review_count: number;
	failed_count: number;
	deferred_count: number;
	priority_band: string | null;
	oldest_backlog_at: number | null;
	provider_unavailable: boolean;
	control_revision: number | null;
	failure_event_id: string | null;
	failure_at: number | null;
	foreground_operation_count: number;
}

export interface LibraryActivityResponse {
	items: LibraryActivityItem[];
}

export type ScanKind = 'incremental' | 'rescan_files' | 'policy_reconcile';

export interface ScanRun {
	id: string;
	kind: ScanKind;
	trigger: 'manual' | 'automatic' | 'subsonic' | 'startup_resume' | 'policy_apply';
	state: LibraryWorkState;
	phase: 'queued' | 'discovering' | 'indexing' | 'reconciling';
	requested_by_user_id: string | null;
	aggregate_scope: string;
	queued_at: number;
	started_at: number | null;
	updated_at: number;
	terminal_at: number | null;
	resume_phase: 'queued' | 'discovering' | 'indexing' | 'reconciling' | null;
	requested_control: 'none' | 'pause' | 'stop';
	terminal_code: string | null;
	coalesced_request_count: number;
	row_revision: number;
	event_revision: number;
	counters: Record<string, number>;
	phase_timings: Record<string, number>;
}

export interface ScanScope {
	root_id: string;
	scope_id: string | null;
	relative_path: string;
	effective_policy: LibraryIdentificationPolicy;
	policy_revision: string;
	estimated_count: number | null;
}

export interface ScanRunSnapshot {
	run: ScanRun;
	scopes: ScanScope[];
	counters: Record<string, number>;
}

export interface ScanRunCurrentResponse {
	active: ScanRun | null;
	queued: ScanRun | null;
}

export interface ScanRunHistoryResponse {
	items: ScanRun[];
	next_cursor: string | null;
}

export interface ScanRunDetailResponse {
	snapshot: ScanRunSnapshot;
}

export interface ScanEstimateResponse {
	approximate: boolean;
	estimated_file_count: number | null;
	estimated_at: number | null;
}

export interface ScanRunRequestedResponse {
	run_id: string;
	disposition: 'started' | 'queued' | 'coalesced' | 'expanded' | 'conflict';
	state: LibraryWorkState;
	row_revision: number;
	queued_reason: string | null;
	conflicting_kind: ScanKind | null;
	estimated_file_count: number | null;
}

export interface ScanControlResponse {
	run_id: string;
	state: LibraryWorkState;
	row_revision: number;
	event_revision: number;
	stream_revision: number;
}

export interface IdentificationControlResponse {
	state: 'running' | 'pausing' | 'paused';
	row_revision: number;
}

export type ReviewState = 'needs_review' | 'keep_tagged' | 'excluded' | 'resolved';

export interface ReviewListItem {
	id: string;
	state: ReviewState;
	reason_code: string;
	local_album_id: string | null;
	local_track_id: string | null;
	album_title: string;
	album_artist_name: string;
	year: number | null;
	track_count: number;
	metadata_incomplete_count: number;
	root_id: string;
	relative_path: string;
	effective_policy: string;
	exclusion_source: string | null;
	release_group_mbid: string | null;
	identity_source: string | null;
	candidate_count: number;
	evidence_summary: Record<string, number>;
	active_job_state: string | null;
	created_at: number;
	updated_at: number;
	row_revision: number;
}

export interface ReviewListResponse {
	items: ReviewListItem[];
	next_cursor: string | null;
	has_more: boolean;
	filtered_total: number;
	counts_by_state: Record<string, number>;
	counts_by_reason: Record<string, number>;
	catalog_revision: number;
}

export interface TrackEvidence {
	local_track_id: string;
	classification: 'supported' | 'unknown' | 'contradictory';
	evidence_kinds: string[];
	candidate_track_title: string | null;
	candidate_disc_number: number | null;
	candidate_track_position: number | null;
	recording_mbid: string | null;
	release_track_mbid: string | null;
}

export interface CandidateEvidence {
	release_group_mbid: string;
	release_mbid: string | null;
	album_title: string;
	album_artist_name: string;
	artist_mbid: string | null;
	release_type: string | null;
	release_date: string | null;
	local_album_title: string;
	local_album_artist_name: string;
	album_title_classification: 'supported' | 'unknown' | 'contradictory';
	album_artist_classification: 'supported' | 'unknown' | 'contradictory';
	track_evidence: TrackEvidence[];
	unmatched_expected_tracks: string[];
	score: number;
	margin: number;
	reason_code: string;
	matcher_version: string;
}

export interface ReviewTrackDetail {
	id: string;
	title: string;
	artist_name: string;
	local_artist_id: string | null;
	relative_path: string;
	disc_number: number;
	track_number: number;
	availability: string;
	membership_locked: boolean;
	recording_mbid: string | null;
}

export interface ReviewHistoryItem {
	id: string;
	kind: 'attempt' | 'decision' | 'action';
	state: string;
	reason_code: string;
	created_at: number;
	actor_user_id: string | null;
}

export interface ReviewDetailResponse {
	review: ReviewListItem;
	tracks: ReviewTrackDetail[];
	current_evidence: CandidateEvidence | null;
	candidates: Array<{
		candidate_key: string;
		evidence_revision: string;
		evidence: CandidateEvidence;
		automatic_safe: boolean;
	}>;
	supported: TrackEvidence[];
	unknown: TrackEvidence[];
	contradictory: TrackEvidence[];
	history: ReviewHistoryItem[];
	available_actions: string[];
	catalog_revision: number;
	album_revision: number | null;
	identity_revision: number | null;
	input_revision: string;
	evidence_revision: string;
	job_revision: number | null;
}

export interface ReviewActionRequest {
	expected_review_revision: number;
	expected_catalog_revision: number;
	expected_identity_revision?: number | null;
	expected_evidence_revision?: string | null;
	idempotency_key?: string | null;
	confirmation?: boolean;
}

export interface CandidateAcceptanceRequest extends ReviewActionRequest {
	candidate_key: string;
	manual_override: boolean;
}

export interface ReviewActionResponse {
	review_id: string;
	state: ReviewState;
	row_revision: number;
	catalog_revision: number;
	action_id: string;
	operation_job_id: string | null;
	remaining_exclusion_source: string | null;
}

export interface BulkReviewSelection {
	review_ids: string[];
	expected_revisions: Record<string, number>;
	normalized_filter: Record<string, string>;
	catalog_revision: number | null;
}

export type BulkReviewAction = 'keep_tagged' | 'retry' | 'exclude' | 'accept_candidate';

export interface BulkReviewPreviewResponse {
	preview_token: string;
	action: string;
	eligible_count: number;
	ineligible_count: number;
	stale_count: number;
	reasons: Record<string, number>;
	album_count: number;
	track_count: number;
	root_count: number;
	crosses_policy_boundaries: boolean;
	estimated_job_count: number;
	playlist_reference_count: number;
	history_reference_count: number;
	requires_local_metadata_confirmation: boolean;
	common_candidate_keys: string[];
}

export type OperationState =
	| 'queued'
	| 'running'
	| 'paused'
	| 'ready'
	| 'succeeded'
	| 'failed'
	| 'cancelled'
	| 'stopped';

export interface OperationWorkResult {
	ordinal: number;
	action: string;
	state: string;
	local_album_id: string | null;
	local_track_id: string | null;
	failure_code: string | null;
	result: Record<string, unknown>;
}

export interface RepairReportSummary {
	total_identities: number;
	remaining_identities: number;
	input_track_count: number;
	playable_after_detach_track_count: number;
	estimated_apply_changes: number;
	catalog_snapshot_revision: number;
	target_matcher_version: string;
	counts_by_finding: Record<string, number>;
	counts_by_reason: Record<string, number>;
	album_counts_by_root: Record<string, number>;
	provider_deferred_count: number;
	failed_evidence_count: number;
	purpose: string;
	ready_album_count: number;
	mapping_candidate_count: number;
	exact_release_required_count: number;
	needs_review_count: number;
}

export interface RepairEstimateResponse {
	identity_count: number;
	selected_root_count: number;
	queued_repair_count: number;
}

export interface IdentityPreparationEstimateResponse {
	album_count: number;
	ready_album_count: number;
	mapping_required_count: number;
	exact_release_required_count: number;
	selected_root_count: number;
	queued_preparation_count: number;
}

export interface OperationResponse {
	id: string;
	kind: string;
	state: OperationState;
	expected_work_count: number;
	completed_count: number;
	succeeded_count: number;
	failed_count: number;
	skipped_count: number;
	control_request: string;
	terminal_code: string | null;
	row_revision: number;
	event_revision: number;
	created_at: number;
	updated_at: number;
	results: OperationWorkResult[];
	results_truncated: boolean;
	repair_summary: RepairReportSummary | null;
	reidentification_candidates: Array<{
		candidate_key: string;
		evidence_revision: string;
		evidence: CandidateEvidence;
		automatic_safe: boolean;
	}>;
}

export interface OperationListResponse {
	items: OperationResponse[];
	next_cursor: string | null;
}

export interface MembershipPreviewResponse {
	preview_token: string;
	source_album_ids: string[];
	target_album_id: string | null;
	track_ids: string[];
	identity_conflicts: string[];
	aliases: string[];
	automatic_groups: Array<{
		local_album_id: string;
		title: string;
		album_artist_name: string;
		track_ids: string[];
		reason_code: string;
	}>;
	reference_counts: Record<string, number>;
}

export interface RepairFindingResponse {
	id: string;
	local_album_id: string;
	evidence_id: string | null;
	review_id: string | null;
	finding_code: string;
	reason_code: string;
	confidence: string;
	apply_eligible: boolean;
	state: string;
	apply_result: string | null;
	updated_at: number;
	row_revision: number;
}

export interface RepairFindingListResponse {
	items: RepairFindingResponse[];
	next_cursor: string | null;
	has_more: boolean;
}

export type LibraryIdentificationPolicy = 'local_metadata' | 'automatic' | 'excluded';

export interface LibraryPathPolicyRule {
	id: string;
	relative_path: string;
	policy: LibraryIdentificationPolicy;
}

export interface LibraryRootSettings {
	id: string;
	path: string;
	label: string;
	policy: LibraryIdentificationPolicy;
	rules: LibraryPathPolicyRule[];
}

export interface TypedLibrarySettings {
	library_roots: LibraryRootSettings[];
	staging_path: string;
	naming_template: string;
	acoustid_api_key: string;
}

export interface TargetLibrarySettingsResponse extends TypedLibrarySettings {
	policy_revision: string;
	reconciliation_required: boolean;
	reconciliation_state: 'applied' | 'awaiting_reconciliation';
	pending_policy_revision: string | null;
	affected_scope_ids: string[];
	actions_applied: string[];
	warnings: string[];
}

export interface LibraryPolicyTreeNode {
	id: string;
	kind: 'root' | 'rule';
	label: string;
	path: string;
	policy: LibraryIdentificationPolicy;
	inherited_from_id: string | null;
	available: boolean;
	indexed_file_count: number | null;
	on_disk_file_count: number | null;
	children: LibraryPolicyTreeNode[];
}

export interface LibraryPolicyTreeResponse {
	policy_revision: string;
	roots: LibraryPolicyTreeNode[];
	warnings: string[];
}

export interface LibraryPolicyImpactResponse {
	current_policy_revision: string;
	proposed_policy_revision: string;
	stale: boolean;
	reconciliation_required: boolean;
	affected_scope_ids: string[];
	indexed_file_count: number | null;
	on_disk_file_count: number | null;
	content_will_become_unavailable: boolean;
	queued_work_will_be_cancelled: boolean;
	warnings: string[];
}

export interface LibraryPolicyApplyPreviewResponse {
	policy_revision: string;
	scope_ids: string[];
	estimated_file_count: number;
	content_will_become_unavailable: boolean;
	queued_work_was_cancelled_on_save: boolean;
}

export interface LibraryPathMappingReport {
	policy_revision: string;
	source_count: number;
	mapped_count: number;
	ambiguous_count: number;
	out_of_root_count: number;
	blocking: boolean;
	items: Array<{
		source_kind: 'library_file' | 'review_row';
		source_id: string;
		absolute_path: string;
		root_id: string | null;
		relative_path: string | null;
		error: 'ambiguous' | 'out_of_root' | null;
	}>;
}
