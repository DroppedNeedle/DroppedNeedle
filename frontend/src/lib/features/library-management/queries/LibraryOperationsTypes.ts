import type { components } from '$lib/api/v3/openapi';

type Schemas = components['schemas'];

export type LibraryActivityItem = Schemas['ActivityItem'];
export type LibraryWorkItem = Schemas['WorkItem'];
export type LibraryActivityResponse = Schemas['LibraryActivity'];

export type ScanKind = Schemas['ScanKind'];
export type ScanPhase = Schemas['ScanPhase'];
export type ScanRun = Schemas['ScanRunView'];
export type ScanRunCurrentResponse = Schemas['ScanRunCurrentResponse'];
export type ScanRunHistoryResponse = Schemas['Page_ScanRunView'];
export type ScanRunDetailResponse = Schemas['RunDetailResponse'];
export type ScanRunFailureItem = Schemas['ScanRunFailureItem'];
export type ScanRunFailuresResponse = Schemas['Page_ScanRunFailureItem'];
export type ScanEstimateResponse = Schemas['ScanEstimateResponse'];
export type ScanRunRequestedResponse = Schemas['ScanRunRequestedResponse'];
export type ScanControlResponse = Schemas['ScanControlResponse'];
export type IdentificationControlResponse = Schemas['IdentificationControlResponse'];

export type CandidateEvidence = Schemas['OperationCandidateEvidenceView'];
export type OperationState = Schemas['OperationState'];
export type RepairReportSummary = Schemas['RepairSummaryView'];
export type RepairEstimateResponse = Schemas['RepairEstimateView'];
export type IdentityPreparationEstimateResponse = Schemas['PreparationEstimateView'];
export type OperationResponse = Schemas['OperationView'];
export type OperationListResponse = Schemas['Page_OperationView'];

// Album split, merge, move and reset grouping previews share one shape;
// an artist merge preview has its own.
export type MembershipPreviewResponse = Schemas['MembershipPreviewResponse'];
export type ArtistMergePreviewResponse = Schemas['ArtistMergePreviewResponse'];
export type CatalogCorrectionResponse = Schemas['CatalogCorrectionResponse'];

export type RepairFindingListResponse = Schemas['RepairFindingListView'];

export type LibraryIdentificationPolicy = Schemas['IdentificationPolicy'];
export type LibraryRootSettings = Schemas['LibraryRoot'];
export type TypedLibrarySettings = Schemas['LibrarySettings'];
export type TargetLibrarySettingsResponse = Schemas['LibrarySettingsResponse'];

export type LibraryPolicyTreeNode = Schemas['LibraryPolicyTreeNode'];
export type LibraryPolicyTreeResponse = Schemas['LibraryPolicyTreeResponse'];
export type LibraryRestorableRoot = Schemas['LibraryRestorableRoot'];
export type LibraryRestorableRootsResponse = Schemas['LibraryRestorableRootsResponse'];
export type LibraryRestoreRootsRequest = Schemas['LibraryRestoreRootsRequest'];
export type LibraryPolicyImpactResponse = Schemas['LibraryPolicyImpactResponse'];
export type LibraryPolicyApplyPreviewResponse = Schemas['LibraryPolicyApplyPreviewResponse'];
