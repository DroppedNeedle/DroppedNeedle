import type { components } from '$lib/api/v3/openapi';

type Schemas = components['schemas'];

// The contract types a stored JSON object as an empty record, so the screens
// that read its keys take it as an open one.
type WithJson<T, K extends keyof T> = Omit<T, K> & {
	[P in K]: T[P] extends unknown[] ? Array<Record<string, unknown>> : Record<string, unknown>;
};

export type ManagementFieldMode = Schemas['FieldMode'];
export type ManagementSelectionKind = Schemas['ManagementSelectionKind'];
export type ManagementEligibility = Schemas['Eligibility'];
export type ManagementChangeKind = 'tags' | 'artwork' | 'path' | 'sidecars' | 'no_change';
export type LibraryManagementTagEditMode = Schemas['ManagementTagEditMode'];
export type ManagementPhase = Schemas['ManagementPhase'];
export type LibraryManagementTagEditValue = string | number | boolean | string[] | null;
export type DuplicateCollisionKind = Schemas['ManagementCollisionKind'];
export type DuplicateResolutionAction = Schemas['ManagementDuplicateAction'];

export type ManagedFieldInfo = Schemas['ManagedFieldInfo'];

export type ArtistCreditSettings = Schemas['ArtistCreditSettings'];

export type RelationshipCreditSettings = Schemas['RelationshipCreditSettings'];

export type MetadataManagementSettings = Schemas['MetadataManagementSettings'];

export type GenreManagementSettings = Schemas['GenreManagementSettings'];

export type ArtworkManagementSettings = Schemas['ArtworkManagementSettings'];

export type PathCompatibilitySettings = Schemas['PathCompatibilitySettings'];

export type OrganizationManagementSettings = Schemas['OrganizationManagementSettings'];

export type EnrichmentManagementSettings = Schemas['EnrichmentManagementSettings'];

export type LibraryManagementProfile = Schemas['LibraryManagementProfile'];

export type ManagementScriptSettings = Schemas['NamingScript'];

export type LibraryManagementRootOverrides = Schemas['LibraryManagementRootOverrides'];

export type LibraryManagementRootAssignment = Schemas['LibraryManagementRootAssignment'];

export type LibraryManagementSettings = Schemas['LibraryManagementSettings'];

export type LibraryManagementSettingsResponse = Schemas['LibraryManagementSettingsResponse'];

export type LibraryManagementChangeImpact = Schemas['LibraryManagementChangeImpact'];

export type LibraryManagementPresetDiff = Schemas['LibraryManagementPresetDiff'];

export type LibraryManagementSelection = Schemas['ManagementSelectionBody'];

export type LibraryManagementPreviewCreatedResponse = Schemas['ManagementPreviewCreatedResponse'];

export type LibraryManagementTagEditorField = Omit<
	Schemas['ManagementTagEditorFieldView'],
	'current_value'
> & {
	current_value: LibraryManagementTagEditValue;
};

export type LibraryManagementTagEditorContext = Omit<
	Schemas['ManagementTagEditorView'],
	'fields'
> & {
	fields: LibraryManagementTagEditorField[];
};

export type LibraryManagementTagEditPreviewRequest = Schemas['ManagementTagEditPreviewBody'];

export type LibraryManagementPreviewDetailResponse = WithJson<
	Schemas['ManagementJobView'],
	'selection'
>;

export type LibraryManagementExternalRefresh = Schemas['ExternalRefresh'];

export type LibraryManagementPlanItem = WithJson<
	Schemas['ManagementPlanItemView'],
	'desired_document' | 'artwork_choices' | 'diff' | 'capability' | 'collisions'
>;

export interface LibraryManagementPlanItemPageResponse {
	items: LibraryManagementPlanItem[];
	next_cursor: string | null;
}

export type LibraryManagementProfileMutationResponse =
	Schemas['LibraryManagementProfileMutationResponse'];

export type LibraryManagementActivationProof = Schemas['ManagementActivationProofBody'];

export type LibraryManagementActivationHealthResponse =
	Schemas['LibraryManagementActivationHealthResponse'];

export type LibraryManagementSettingsUpdateRequest = Schemas['LibraryManagementSaveRequest'];

export type LibraryManagementSettingsImpactRequest =
	Schemas['LibraryManagementSettingsImpactRequest'];

export type LibraryManagementProfileCopyRequest = Schemas['LibraryManagementProfileCopyRequest'];

export type LibraryManagementProfileDeleteRequest =
	Schemas['LibraryManagementProfileDeleteRequest'];

export type LibraryManagementProfileExportRequest =
	Schemas['LibraryManagementProfileExportRequest'];

export type LibraryManagementProfileExportResponse =
	Schemas['LibraryManagementProfileExportResponse'];

export type LibraryManagementProfileImportPreviewRequest =
	Schemas['LibraryManagementProfileImportPreviewRequest'];

export type LibraryManagementProfileImportRequest =
	Schemas['LibraryManagementProfileImportRequest'];

export type LibraryManagementProfileImportWarning =
	Schemas['LibraryManagementProfileImportWarning'];

export type LibraryManagementProfileImportPreviewResponse =
	Schemas['LibraryManagementProfileImportPreviewResponse'];

export type LibraryManagementProfileImportResponse =
	Schemas['LibraryManagementProfileImportResponse'];

export type LibraryManagementPreviewCreateRequest = Schemas['ManagementPreviewCreateBody'];
export type LibraryManagementActivationPreviewRequest = Schemas['ManagementActivationPreviewBody'];
export type LibraryManagementActivationConfirmRequest = Schemas['ManagementActivationConfirmBody'];
export type LibraryManagementApplyRequest = Schemas['ManagementApplyBody'];
export type LibraryManagementPreviewReissueResponse = Schemas['ManagementPreviewReissueResponse'];
export type LibraryManagementDiscardRequest = Schemas['ManagementDiscardBody'];
export type LibraryManagementUndoPreviewRequest = Schemas['ManagementUndoPreviewBody'];
export type LibraryManagementBaselineRestorePreviewRequest =
	Schemas['ManagementRestorePreviewBody'];
export type LibraryManagementDuplicateResolutionPreviewRequest = Schemas['ManagementDuplicateBody'];
export type LibraryManagementBaselinePurgeRequest = Schemas['ManagementPurgeBody'];

export type LibraryManagementResultItem = Omit<
	Schemas['ManagementResultItemView'],
	'plan' | 'result'
> & {
	plan: LibraryManagementPlanItem;
	result: Record<string, unknown>;
};

export interface LibraryManagementResultPageResponse {
	items: LibraryManagementResultItem[];
	next_cursor: string | null;
}

export type LibraryManagementOperationHistoryItem = WithJson<
	Schemas['ManagementHistoryItemView'],
	'selection'
>;

export interface LibraryManagementOperationHistoryResponse {
	items: LibraryManagementOperationHistoryItem[];
	next_cursor: string | null;
}

export type LibraryManagementRecoveryDiagnosticsResponse = Schemas['ManagementRecoveryView'];

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
	cursor?: string;
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
