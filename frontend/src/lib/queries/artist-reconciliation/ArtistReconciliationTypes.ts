import type { components } from '$lib/api/v3/openapi';

type Schemas = components['schemas'];

export type ArtistReconciliationGroupState = Schemas['ArtistGroupState'];
export type ArtistReconciliationProgress = Schemas['ArtistReconciliationProgress'];
export type ArtistReconciliationMember = Schemas['ArtistReconciliationMember'];
export type ArtistDuplicateGroupSummary = Schemas['ArtistDuplicateGroupSummary'];
export type ArtistDuplicateGroupListResponse = Schemas['ArtistDuplicateGroupListResponse'];
export type ArtistCreditEvidence = Schemas['ArtistCreditEvidence'];
export type ArtistOwnedReference = Schemas['ArtistOwnedReference'];
export type ArtistDuplicateGroupDetail = Schemas['ArtistDuplicateGroupDetail'];
export type ArtistDuplicateGroupDismissResponse = Schemas['ArtistDuplicateGroupDismissResponse'];

export interface ArtistDuplicateGroupParams {
	state?: ArtistReconciliationGroupState;
	search?: string;
}
