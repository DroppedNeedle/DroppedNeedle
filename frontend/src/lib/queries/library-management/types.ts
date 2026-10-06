import type { components } from '$lib/api/v3/openapi';

// Settings, profile and sharing shapes are the generated v3 schemas. The
// preview, operation, recovery and tag-editor shapes have no v3 route yet
// and stay hand-written until they do.
export type ManagementFieldMode = components['schemas']['FieldMode'];
export type ManagementGenreMode = components['schemas']['GenreMode'];
export type ManagementSelectionKind = 'roots' | 'artists' | 'albums' | 'tracks' | 'filter';
export type ManagementEligibility = 'eligible' | 'warning' | 'blocked' | 'stale';
export type ManagementChangeKind = 'tags' | 'artwork' | 'path' | 'sidecars' | 'no_change';
export type LibraryManagementTagEditMode = 'save_override' | 'write_once' | 'reset_canonical';
export type LibraryManagementTagEditValue = string | number | boolean | string[] | null;
export type DuplicateCollisionKind =
	| 'same_path_same_content'
	| 'same_path_different_content'
	| 'same_release_position_different_content'
	| 'normalized_path_collision'
	| 'sidecar_collision'
	| 'destination_created_after_preview';
export type DuplicateResolutionAction =
	| 'keep_existing'
	| 'keep_incoming_alternate'
	| 'recycle_existing_keep_incoming'
	| 'recycle_incoming_keep_existing';

export type ManagedFieldSettings = components['schemas']['ManagedField'];

export type ArtistCreditSettings = components['schemas']['ArtistCreditSettings'];

export type RelationshipCreditSettings = components['schemas']['RelationshipCreditSettings'];

export type FormatCompatibilitySettings = components['schemas']['FormatCompatibilitySettings'];

export type MetadataManagementSettings = components['schemas']['MetadataManagementSettings'];

export type GenreAliasSettings = components['schemas']['GenreAlias'];

export type GenreManagementSettings = components['schemas']['GenreManagementSettings'];

export type ArtworkProvider = components['schemas']['ArtworkProvider'];
export type ArtworkImageType = components['schemas']['ArtworkImageType'];

export type ArtworkManagementSettings = components['schemas']['ArtworkManagementSettings'];

export type PathCompatibilitySettings = components['schemas']['PathCompatibilitySettings'];

export type OrganizationManagementSettings =
	components['schemas']['OrganizationManagementSettings'];

export type FileBehaviorSettings = components['schemas']['FileBehaviorSettings'];

export type EnrichmentManagementSettings = components['schemas']['EnrichmentManagementSettings'];

export type LibraryManagementProfile = components['schemas']['LibraryManagementProfile'];

export interface ManagementScriptSettings {
	id: string;
	name: string;
	source: string;
	revision: string;
	preset_origin: string | null;
	preset_version: number | null;
}

export type LibraryManagementRootOverrides =
	components['schemas']['LibraryManagementRootOverrides'];

export type LibraryManagementRootAssignment =
	components['schemas']['LibraryManagementRootAssignment'];

export type LibraryManagementSettings = components['schemas']['LibraryManagementSettings'];

export type LibraryManagementSettingsResponse =
	components['schemas']['LibraryManagementSettingsResponse'];

export type LibraryManagementChangeImpact = components['schemas']['LibraryManagementChangeImpact'];

export type LibraryManagementPresetDiff = components['schemas']['LibraryManagementPresetDiff'];

export interface LibraryManagementCatalogFilter {
	search?: string | null;
	genre?: string | null;
	from_year?: number | null;
	to_year?: number | null;
	artist_ids?: string[];
	album_artist_only?: boolean;
}

export interface LibraryManagementSelection {
	kind: ManagementSelectionKind;
	ids?: string[];
	catalog_filter?: LibraryManagementCatalogFilter | null;
}

export interface LibraryManagementPreviewCreatedResponse {
	job_id: string;
	preview_token: string;
	created_at: number;
	expires_at: number;
	existing: boolean;
}

export interface LibraryManagementTagEditorField {
	field_name: string;
	scope: 'album' | 'track';
	cardinality: 'string' | 'integer' | 'boolean' | 'ordered_strings';
	current_value: LibraryManagementTagEditValue;
	override_id: string | null;
	override_mode: 'replace' | 'preserve' | 'clear' | null;
	override_row_revision: number | null;
}

export interface LibraryManagementTagEditorContext {
	local_track_id: string;
	local_album_id: string;
	root_id: string;
	profile_id: string;
	profile_name: string;
	settings_revision: string;
	policy_revision: string;
	track_revision: number;
	album_revision: number;
	accepted_identity: boolean;
	identity_reason: string | null;
	fields: LibraryManagementTagEditorField[];
}

export interface LibraryManagementTagEditPreviewRequest {
	local_track_id: string;
	mode: LibraryManagementTagEditMode;
	expected_settings_revision: string;
	expected_policy_revision: string;
	fields: Array<{
		field_name: string;
		value?: LibraryManagementTagEditValue;
	}>;
	idempotency_key?: string | null;
}

export interface LibraryManagementPreviewSummary {
	selected_item_count: number | null;
	item_count: number;
	bundle_count: number;
	eligible_count: number;
	warning_count: number;
	blocked_count: number;
	stale_count: number;
	no_change_count: number;
	tag_change_count: number;
	artwork_change_count: number;
	path_change_count: number;
	sidecar_change_count: number;
	estimated_temporary_bytes: number;
	expanded_track_count: number;
	reasons: Record<string, number>;
	roots: Record<string, number>;
	formats: Record<string, number>;
	deferred_sources: Record<string, number>;
	metadata_snapshot_ids: string[];
}

export interface LibraryManagementPreviewDetailResponse {
	job_id: string;
	state: string;
	phase: string;
	mode: string;
	origin: string;
	profile_id: string;
	profile_name: string;
	profile_revision: string;
	settings_revision: string;
	policy_revision: string;
	catalog_revision: number;
	proposed_settings_revision: string | null;
	target_root_id: string | null;
	selection: Record<string, unknown>;
	summary: LibraryManagementPreviewSummary;
	created_at: number;
	updated_at: number;
	expires_at: number | null;
	expired: boolean;
	stale: boolean;
	stale_reasons: string[];
	stale_input_count: number;
	stale_sample_relative_paths: string[];
	ready_for_confirmation: boolean;
	operation_row_revision: number;
	operation_event_revision: number;
	terminal_code: string | null;
	worker_heartbeat_at: number | null;
	worker_lease_expires_at: number | null;
	worker_stalled: boolean;
	expected_work_count: number;
	completed_count: number;
	succeeded_count: number;
	failed_count: number;
	skipped_count: number;
	control_request: string;
	undo_available_count: number;
	undo_expired_count: number;
	undo_expires_at: number | null;
	baseline_available_count: number;
	external_refreshes: LibraryManagementExternalRefreshDelivery[];
}

export interface LibraryManagementExternalRefreshDelivery {
	target: 'plex' | 'jellyfin' | 'navidrome';
	state: 'pending' | 'delivering' | 'retry_wait' | 'succeeded' | 'failed' | 'unavailable';
	attempts: number;
	max_attempts: number;
	failure_code: string | null;
	updated_at: number;
	completed_at: number | null;
}

export interface LibraryManagementPlanItem {
	ordinal: number;
	bundle_ordinal: number;
	local_album_id: string | null;
	local_track_id: string | null;
	source_root_id: string | null;
	source_relative_path: string | null;
	destination_root_id: string | null;
	destination_relative_path: string | null;
	eligibility: ManagementEligibility;
	reason_code: string | null;
	estimated_temporary_bytes: number;
	desired_document: Record<string, unknown>;
	artwork_choices: Array<Record<string, unknown>>;
	diff: Record<string, unknown>;
	capability: Record<string, unknown>;
	collisions: Array<Record<string, unknown>>;
}

export interface LibraryManagementPlanItemPageResponse {
	items: LibraryManagementPlanItem[];
	next_after_ordinal: number | null;
	has_more: boolean;
}

export type LibraryManagementProfileMutationResponse =
	components['schemas']['LibraryManagementProfileMutationResponse'];

export interface LibraryManagementActivationProof {
	root_id: string;
	job_id: string;
	preview_token: string;
}

export type LibraryManagementActivationHealthResponse =
	components['schemas']['LibraryManagementActivationHealthResponse'];

export type LibraryManagementSettingsUpdateRequest =
	components['schemas']['LibraryManagementSaveRequest'];

export type LibraryManagementSettingsImpactRequest =
	components['schemas']['LibraryManagementSettingsImpactRequest'];

export type LibraryManagementProfileCreateRequest =
	components['schemas']['LibraryManagementProfileCreateRequest'];

export type LibraryManagementProfileCopyRequest =
	components['schemas']['LibraryManagementProfileCopyRequest'];

export type LibraryManagementProfileUpdateRequest =
	components['schemas']['LibraryManagementProfileUpdateRequest'];

export type LibraryManagementProfileDeleteRequest =
	components['schemas']['LibraryManagementProfileDeleteRequest'];

export type LibraryManagementProfileExportRequest =
	components['schemas']['LibraryManagementProfileExportRequest'];

export type LibraryManagementProfileExportResponse =
	components['schemas']['LibraryManagementProfileExportResponse'];

export type LibraryManagementProfileImportPreviewRequest =
	components['schemas']['LibraryManagementProfileImportPreviewRequest'];

export type LibraryManagementProfileImportRequest =
	components['schemas']['LibraryManagementProfileImportRequest'];

export type LibraryManagementProfileImportWarning =
	components['schemas']['LibraryManagementProfileImportWarning'];

export type LibraryManagementProfileImportPreviewResponse =
	components['schemas']['LibraryManagementProfileImportPreviewResponse'];

export type LibraryManagementProfileImportResponse =
	components['schemas']['LibraryManagementProfileImportResponse'];

export interface LibraryManagementPreviewCreateRequest {
	selection: LibraryManagementSelection;
	profile_id: string;
	expected_settings_revision: string;
	expected_policy_revision: string;
	idempotency_key?: string | null;
	target_root_id?: string | null;
	overrides?: LibraryManagementRootOverrides | null;
}

export interface LibraryManagementActivationPreviewRequest {
	root_id: string;
	settings: LibraryManagementSettings;
	expected_settings_revision: string;
	expected_policy_revision: string;
	idempotency_key?: string | null;
}

export interface LibraryManagementActivationConfirmRequest {
	settings: LibraryManagementSettings;
	proofs: LibraryManagementActivationProof[];
	expected_settings_revision: string;
	confirmation?: boolean;
}

export interface LibraryManagementApplyRequest {
	preview_token: string;
	expected_operation_row_revision: number;
	idempotency_key: string;
	confirmation?: boolean;
}

export interface LibraryManagementPreviewReissueResponse {
	job_id: string;
	preview_token: string;
	created_at: number;
	expires_at: number;
}

export interface LibraryManagementDiscardRequest {
	expected_operation_row_revision: number;
}

export interface LibraryManagementUndoPreviewRequest {
	expected_operation_row_revision: number;
	idempotency_key: string;
}

export interface LibraryManagementBaselineRestorePreviewRequest {
	selection: LibraryManagementSelection;
	expected_settings_revision: string;
	expected_policy_revision: string;
	idempotency_key: string;
}

export interface LibraryManagementDuplicateResolutionPreviewRequest {
	source_job_id: string;
	source_plan_item_ordinal: number;
	expected_source_operation_row_revision: number;
	collision_kind: DuplicateCollisionKind;
	existing_root_id: string;
	existing_relative_path: string;
	action: DuplicateResolutionAction;
	expected_settings_revision: string;
	expected_policy_revision: string;
	idempotency_key: string;
	existing_local_track_id?: string | null;
	alternate_relative_path?: string | null;
}

export interface LibraryManagementBaselinePurgeImpactResponse {
	baseline_count: number;
	referenced_blob_count: number;
	referenced_blob_bytes: number;
	blocked_journal_count: number;
	active_restore_count: number;
	catalog_revision: number;
	impact_token: string;
}

export interface LibraryManagementBaselinePurgeRequest {
	impact_token: string;
	expected_catalog_revision: number;
	typed_confirmation: string;
	idempotency_key: string;
}

export interface LibraryManagementBaselinePurgeResponse {
	purged_baseline_count: number;
	detached_reference_count: number;
	cleaned_blob_count: number;
	existing: boolean;
}

export interface LibraryManagementResultItem {
	plan: LibraryManagementPlanItem;
	work_state: string;
	failure_code: string | null;
	result: Record<string, unknown>;
	journal_states: string[];
}

export interface LibraryManagementResultPageResponse {
	items: LibraryManagementResultItem[];
	next_after_ordinal: number | null;
	has_more: boolean;
}

export interface LibraryManagementOperationHistoryItem {
	operation: import('$lib/queries/library/LibraryOperationsTypes').OperationResponse;
	mode: string;
	origin: string;
	phase: string;
	profile_id: string;
	profile_name: string;
	profile_revision: string;
	target_root_id: string | null;
	activation_preview: boolean;
	selection: Record<string, unknown>;
	eligible_count: number;
	warning_count: number;
	blocked_count: number;
	expires_at: number | null;
}

export interface LibraryManagementOperationHistoryResponse {
	items: LibraryManagementOperationHistoryItem[];
	next_cursor: string | null;
}

export interface LibraryManagementRecoveryDiagnosticsResponse {
	recoverable_bundle_count: number;
	nonterminal_journal_count: number;
	needs_attention_count: number;
	cleanup_pending_count: number;
	oldest_updated_at: number | null;
	state_counts: Record<string, number>;
	needs_attention_bundles?: LibraryManagementNeedsAttentionBundle[];
}

export interface LibraryManagementNeedsAttentionBundle {
	bundle_id: string;
}

export interface LibraryManagementImportBundleResolveResponse {
	bundle_id: string;
	state: string;
	verified_files: number;
	total_files: number;
}

export interface LibraryManagementHistoryParams {
	limit?: number;
	cursor?: string;
	origin?: string;
	profileId?: string;
	rootId?: string;
	state?: string;
	mode?: string;
	createdFrom?: number;
	createdTo?: number;
}

export interface LibraryManagementPlanItemParams {
	afterOrdinal?: number;
	limit?: number;
	eligibility?: ManagementEligibility;
	reasonCode?: string;
	rootId?: string;
	artistId?: string;
	albumId?: string;
	audioFormat?: string;
	collisionClass?: string;
	hasPreservedValue?: boolean;
	hasRepresentationLoss?: boolean;
	changeKind?: ManagementChangeKind;
}
