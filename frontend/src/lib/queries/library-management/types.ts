export type ManagementFieldMode = 'disabled' | 'replace' | 'fill_missing' | 'merge' | 'preserve';
export type ManagementGenreMode = 'replace' | 'merge' | 'fill_missing';
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

export interface ManagedFieldSettings {
	field: string;
	mode: ManagementFieldMode;
	clear_when_canonical_missing: boolean;
}

export interface ArtistCreditSettings {
	standardization: 'credited' | 'variations' | 'canonical';
	translate_names: boolean;
	preferred_locales: string[];
}

export interface RelationshipCreditSettings {
	enabled: boolean;
	types: Array<
		| 'composer'
		| 'lyricist'
		| 'conductor'
		| 'performer'
		| 'arranger'
		| 'remixer'
		| 'producer'
		| 'other'
	>;
}

export interface FormatCompatibilitySettings {
	id3_version: '2.4' | '2.3';
	id3v23_join_delimiter: string;
	id3_text_encoding: 'utf8' | 'utf16';
	remove_id3_from_flac: boolean;
	mp3_apev2_policy: 'preserve' | 'remove';
	raw_aac_tag_policy: 'save_apev2' | 'do_not_write' | 'remove_apev2';
	wav_tag_policy: 'id3' | 'riff_info' | 'preserve_existing';
	constrained_genres_primary_only: boolean;
}

export interface MetadataManagementSettings {
	enabled: boolean;
	fields: ManagedFieldSettings[];
	artist_credits: ArtistCreditSettings;
	relationships: RelationshipCreditSettings;
	tagging_script_ids: string[];
	preserve_fields: string[];
	scrub_unmanaged_tags: boolean;
	preserve_embedded_art_during_scrub: boolean;
	format_compatibility: FormatCompatibilitySettings;
}

export interface GenreAliasSettings {
	source: string;
	target: string;
}

export interface GenreManagementSettings {
	enabled: boolean;
	mode: ManagementGenreMode;
	sources: Array<'musicbrainz' | 'listenbrainz' | 'lastfm' | 'existing_local'>;
	maximum_count: number;
	musicbrainz_minimum_count: number;
	listenbrainz_minimum_count: number;
	lastfm_minimum_weight: number;
	listenbrainz_curated_only: boolean;
	lastfm_whitelist_only: boolean;
	canonicalize: boolean;
	maximum_ancestry_depth: number;
	allowlist: string[];
	denylist: string[];
	aliases: GenreAliasSettings[];
	preferred_casing: string[];
	write_primary_only_for_constrained_formats: boolean;
}

export type ArtworkProvider =
	| 'cover_art_archive_release'
	| 'cover_art_archive_release_group'
	| 'local_files'
	| 'embedded'
	| 'audiodb';
export type ArtworkImageType =
	| 'front'
	| 'back'
	| 'booklet'
	| 'medium'
	| 'tray'
	| 'obi'
	| 'spine'
	| 'track'
	| 'other';

export interface ArtworkManagementSettings {
	embedded_enabled: boolean;
	external_enabled: boolean;
	providers: ArtworkProvider[];
	approved_only: boolean;
	download_size: 'full' | '1200' | '500' | '250';
	local_file_patterns: string[];
	image_types: ArtworkImageType[];
	minimum_width: number;
	minimum_height: number;
	embedded_maximum_size: number;
	embedded_format: 'original' | 'jpeg' | 'png' | 'webp';
	external_maximum_size: number;
	external_format: 'original' | 'jpeg' | 'png' | 'webp';
	embedded_front_only: boolean;
	external_front_only: boolean;
	never_replace_with_smaller: boolean;
	preserve_existing_types: ArtworkImageType[];
	external_naming_script_id: string | null;
	overwrite_external_files: boolean;
}

export interface PathCompatibilitySettings {
	windows_compatible: boolean;
	replace_non_ascii: boolean;
	replace_spaces_with_underscores: boolean;
	separator_replacement: string;
	maximum_component_length: number;
	maximum_path_length: number;
	unicode_normalization: 'NFC' | 'NFKC';
	extension_case: 'preserve' | 'lower' | 'upper';
	windows_legacy_path_limit: boolean;
}

export interface OrganizationManagementSettings {
	rename_enabled: boolean;
	move_enabled: boolean;
	naming_script_id: string;
	compatibility: PathCompatibilitySettings;
	move_sidecars: boolean;
	sidecar_patterns: string[];
	source_cleanup: 'keep' | 'remove_after_confirmed_move';
	remove_empty_directories: boolean;
}

export interface FileBehaviorSettings {
	preserve_timestamps: boolean;
	preserve_permissions: boolean;
	strict_capability_gate: boolean;
	reject_symlinks: boolean;
	validate_written_metadata: boolean;
	validate_technical_audio: boolean;
}

export interface EnrichmentManagementSettings {
	lyrics: {
		enabled: boolean;
		provider: 'lrclib';
		write_plain: boolean;
		write_synced: boolean;
		required: boolean;
	};
	replaygain: {
		enabled: boolean;
		mode: 'preserve' | 'fill_missing' | 'replace';
		album_aware: boolean;
		required: boolean;
	};
}

export interface LibraryManagementProfile {
	id: string;
	name: string;
	description: string;
	preset_origin: string | null;
	preset_version: number | null;
	revision: string;
	metadata: MetadataManagementSettings;
	genres: GenreManagementSettings;
	artwork: ArtworkManagementSettings;
	organization: OrganizationManagementSettings;
	file_behavior: FileBehaviorSettings;
	enrichment: EnrichmentManagementSettings;
	notification: {
		refresh_droppedneedle: boolean;
		refresh_external_servers: boolean;
	};
}

export interface ManagementScriptSettings {
	id: string;
	name: string;
	source: string;
	revision: string;
	preset_origin: string | null;
	preset_version: number | null;
}

export interface LibraryManagementRootOverrides {
	metadata_enabled: boolean | null;
	genres_enabled: boolean | null;
	embedded_artwork_enabled: boolean | null;
	external_artwork_enabled: boolean | null;
	rename_enabled: boolean | null;
	move_enabled: boolean | null;
	move_sidecars: boolean | null;
	source_cleanup: 'keep' | 'remove_after_confirmed_move' | null;
	preserve_timestamps: boolean | null;
	naming_script_id: string | null;
}

export interface LibraryManagementRootAssignment {
	root_id: string;
	profile_id: string | null;
	overrides: LibraryManagementRootOverrides | null;
	enabled: boolean;
	automatic_acquisitions: boolean;
	automatic_drop_imports: boolean;
	automatic_scan_discovered: boolean;
	activation_profile_revision: string | null;
	activation_policy_revision: string | null;
	activation_settings_revision: string | null;
	activation_preview_token: string | null;
	activation_preview_hash: string | null;
	activation_confirmed_at: number | null;
}

export interface LibraryManagementSettings {
	schema_version: number;
	profiles: LibraryManagementProfile[];
	default_profile_id: string;
	root_assignments: LibraryManagementRootAssignment[];
	naming_scripts: ManagementScriptSettings[];
	tagging_scripts: ManagementScriptSettings[];
	undo_retention_days: number;
	preview_retention_hours: number;
	recycle_bin_path: string;
	external_refresh: {
		enabled: boolean;
		plex_enabled: boolean;
		jellyfin_enabled: boolean;
		navidrome_enabled: boolean;
		retry_attempts: number;
		retry_delay_seconds: number;
	};
}

export interface LibraryManagementSettingsResponse extends LibraryManagementSettings {
	settings_revision: string;
}

export interface LibraryManagementChangeImpact {
	current_settings_revision: string;
	proposed_settings_revision: string;
	stale: boolean;
	classification: 'no_change' | 'harmless' | 'restrictive' | 'destructive';
	preview_required: boolean;
	affected_root_ids: string[];
	reasons: string[];
}

export interface LibraryManagementPresetDiff {
	profile_id: string;
	preset_origin: string | null;
	preset_version: number | null;
	differs: boolean;
	changed_groups: string[];
	preset_profile: LibraryManagementProfile | null;
}

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
	ready_for_confirmation: boolean;
	operation_row_revision: number;
	operation_event_revision: number;
	terminal_code: string | null;
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

export interface LibraryManagementProfileMutationResponse {
	profile: LibraryManagementProfile;
	settings_revision: string;
}

export interface LibraryManagementActivationProof {
	root_id: string;
	job_id: string;
	preview_token: string;
}

export interface LibraryManagementSettingsUpdateRequest {
	settings: LibraryManagementSettings;
	expected_settings_revision: string;
}

export interface LibraryManagementSettingsImpactRequest {
	settings: LibraryManagementSettings;
	expected_settings_revision?: string | null;
}

export interface LibraryManagementProfileCreateRequest {
	name: string;
	description?: string;
	expected_settings_revision: string;
}

export interface LibraryManagementProfileCopyRequest {
	name: string;
	expected_settings_revision: string;
}

export interface LibraryManagementProfileUpdateRequest {
	profile: LibraryManagementProfile;
	expected_settings_revision: string;
}

export interface LibraryManagementProfileDeleteRequest {
	expected_settings_revision: string;
}

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
	selection: Record<string, unknown>;
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
