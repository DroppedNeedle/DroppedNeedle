import type { components } from '$lib/api/v3/openapi';

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

export interface DeferredJobSummary {
	job_id: string;
	local_album_id: string | null;
	album_title: string | null;
	artist_name: string | null;
	last_failure_code: string;
	attempt_count: number;
	not_before: number | null;
	updated_at: number;
}

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
	deferred_reason_counts: Record<string, number>;
	deferred_jobs: DeferredJobSummary[];
	attention_count: number;
	priority_band: string | null;
	oldest_backlog_at: number | null;
	provider_unavailable: boolean;
	control_revision: number | null;
	failure_event_id: string | null;
	failure_at: number | null;
	foreground_operation_count: number;
}

export type LibraryWorkKind =
	| 'scan'
	| 'identification'
	| 'identity_preparation'
	| 'reidentification'
	| 'identity_review'
	| 'maintenance'
	| 'library_management'
	| 'recovery';

export interface LibraryWorkItem {
	id: string;
	kind: LibraryWorkKind;
	state: string;
	phase: string | null;
	mode: string | null;
	effect: 'catalog_only' | 'file_writing' | 'attention';
	processed: number;
	total: number | null;
	unit: 'files' | 'albums' | 'releases' | 'items';
	indeterminate: boolean;
	remaining_count: number | null;
	subject_count: number | null;
	started_at: number | null;
	updated_at: number;
	origin: string | null;
	profile_name: string | null;
	scope_label: string | null;
	new_count: number;
	changed_count: number;
	missing_count: number;
	warning_count: number;
	blocked_count: number;
	succeeded_count: number;
	failed_count: number;
	skipped_count: number;
	priority: number;
	failure_event_id: string | null;
	failure_at: number | null;
	/** Presentation-only synthetic drain marker (id 'identification-drain', never a real run). */
	synthetic?: boolean;
	/** False while a settled scan still has albums awaiting identification. */
	catalog_settled?: boolean | null;
	/** Albums still awaiting identification when the synthetic drain is active. */
	pending_identification?: number | null;
}

export interface LibraryActivityResponse {
	items: LibraryActivityItem[];
	work_items: LibraryWorkItem[];
	revisions?: Record<string, number>;
}

export type ScanKind = components['schemas']['ScanKind'];
export type ScanPhase = components['schemas']['ScanPhase'];

export interface ScanRun {
	id: string;
	kind: ScanKind;
	trigger: 'manual' | 'automatic' | 'subsonic' | 'startup_resume' | 'policy_apply';
	state: LibraryWorkState;
	phase: ScanPhase;
	requested_by_user_id: string | null;
	aggregate_scope: components['schemas']['AggregateScope'];
	queued_at: number;
	started_at: number | null;
	updated_at: number;
	terminal_at: number | null;
	resume_phase: ScanPhase | null;
	requested_control: 'none' | 'pause' | 'stop';
	terminal_code: string | null;
	/** What the terminal code means, in plain words. */
	terminal_message?: string | null;
	/** What to do about the terminal code. */
	terminal_action?: string | null;
	coalesced_request_count: number;
	row_revision: number;
	event_revision: number;
	counters: Record<string, number>;
	phase_timings: Record<string, number>;
	/** False hides the diagnostics export for this run. */
	diagnostics_available?: boolean;
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

export interface ScanRunFailureItem {
	root_id: string;
	relative_path: string;
	failure_code: string;
	/** What happened, in plain words. */
	message: string;
	/** What to do about it. */
	action: string;
	failure_detail: string;
	phase: 'discovering' | 'indexing' | 'reconciling';
	recorded_at: number;
}

export interface ScanRunFailuresResponse {
	items: ScanRunFailureItem[];
	next_cursor: number | null;
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

export interface TrackEvidence {
	local_track_id: string;
	classification: 'supported' | 'unknown' | 'contradictory';
	evidence_kinds: string[];
	candidate_track_title: string | null;
	candidate_disc_number: number | null;
	candidate_track_position: number | null;
	recording_mbid: string | null;
	release_track_mbid: string | null;
	recording_mbid_redirects?: string[];
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

export type OperationState = components['schemas']['OperationState'];

export interface OperationWorkResult {
	ordinal: number;
	action: string;
	state: components['schemas']['WorkState'];
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
	purpose: components['schemas']['Purpose'];
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
	control_request: components['schemas']['ControlRequest'];
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
	selected_reidentification_candidate_key: string | null;
}

export interface OperationListResponse {
	items: OperationResponse[];
	next_cursor: string | null;
}

// Album split, merge, move and reset grouping previews share one shape;
// an artist merge preview has its own.
export type MembershipPreviewResponse = components['schemas']['MembershipPreviewResponse'];
export type ArtistMergePreviewResponse = components['schemas']['ArtistMergePreviewResponse'];
export type CatalogCorrectionResponse = components['schemas']['CatalogCorrectionResponse'];
export type EditionChange = components['schemas']['EditionChangeView'];

export type SuggestedEditionSummary = components['schemas']['SuggestedEditionView'];
export type RepairFindingResponse = components['schemas']['RepairFindingView'];
export type AutomaticEditionUndoInfo = components['schemas']['AutomaticEditionUndoInfoView'];
export type AutomaticEditionUndoResponse = components['schemas']['AutomaticEditionUndoResponse'];
export type RepairFindingListResponse = components['schemas']['RepairFindingListView'];

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
	naming_script: string;
	acoustid_api_key: string;
	enabled: boolean;
}

export interface TargetLibrarySettingsResponse extends TypedLibrarySettings {
	policy_revision: string;
	reconciliation_required: boolean;
	reconciliation_state: 'applied' | 'awaiting_reconciliation';
	pending_policy_revision: string | null;
	affected_scope_ids: string[];
	actions_applied: string[];
	warnings: string[];
	naming_example: string | null;
}

// The path policy routes are on v3; their shapes come from the contract.
type PolicySchemas = components['schemas'];
export type LibraryPolicyTreeNode = PolicySchemas['LibraryPolicyTreeNode'];
export type LibraryPolicyTreeResponse = PolicySchemas['LibraryPolicyTreeResponse'];
export type LibraryRestorableRoot = PolicySchemas['LibraryRestorableRoot'];
export type LibraryRestorableRootsResponse = PolicySchemas['LibraryRestorableRootsResponse'];
export type LibraryRestoreRootsRequest = PolicySchemas['LibraryRestoreRootsRequest'];
export type LibraryPolicyImpactResponse = PolicySchemas['LibraryPolicyImpactResponse'];
export type LibraryPolicyApplyPreviewResponse = PolicySchemas['LibraryPolicyApplyPreviewResponse'];
export type LibraryPathMappingReport = PolicySchemas['LibraryPathMappingReport'];
