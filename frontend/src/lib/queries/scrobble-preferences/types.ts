import type { components } from '$lib/api/v3/openapi';

// how a user's listening appears to others in the now-playing feed
export type NowPlayingVisibility = 'full' | 'track_hidden' | 'offline';

// standing-grant state for Weekly Mix auto-request (admins read 'approved' by role)
export type AutoRequestState = 'none' | 'pending' | 'approved' | 'rejected' | 'revoked';

// shapes come straight from the generated v3 contract (no hand mirror)
export type ScrobblePreferences = components['schemas']['ScrobblePreferences'];
// The generator marks every update field required (openapi-typescript treats
// defaulted props as always present), but the route is a partial update:
// absent fields keep their stored values (#[serde(default)] on the backend
// struct), so callers send only what changed.
export type ScrobblePreferencesUpdate = Partial<components['schemas']['ScrobblePreferencesUpdate']>;

// the build runs in the background; the outcome arrives on the per-user SSE
// stream as a personal_mix_refreshed event
export type PersonalMixRefreshResponse = components['schemas']['RequestsRefreshResponse'];

// Personal-mix rows come from the generated v3 contract. v3 always resolves
// `user_name`.
export type PersonalMixApprovalItem = components['schemas']['PersonalMixApprovalItem'];
export type PersonalMixApprovalsResponse = components['schemas']['PersonalMixApprovalsResponse'];
export type ApprovalActionResponse = components['schemas']['ActionResponse'];
