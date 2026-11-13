import { ScrobblePreferencesQueryKeyFactory } from '$lib/queries/featureKeys';
import type { components } from '$lib/api/v3/openapi';

import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { createQuery, queryOptions } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import {
	createAppMutation,
	pendingApprovalsChanged,
	personalMixApprovalsChanged,
	recommendationSourcesChanged
} from '$lib/queries/effects';

// how a user's listening appears to others in the now-playing feed
export type NowPlayingVisibility = components['schemas']['NowPlayingVisibility'];

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

type Getter<T> = () => T;

export const getPersonalMixApprovalsQuery = (getEnabled: Getter<boolean>) =>
	createQuery(() => ({
		queryKey: ScrobblePreferencesQueryKeyFactory.personalMixApprovals(),
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/requests/personal-mix-approvals'), {
				signal
			}),
		enabled: getEnabled()
	}));

const SCROBBLE_PREFERENCES_TIMEOUT_MS = 10_000;

export const getScrobblePreferencesQueryOptions = (userId: string | undefined) =>
	queryOptions({
		queryKey: ScrobblePreferencesQueryKeyFactory.get(userId),
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/me/scrobble-preferences'), {
				signal,
				timeoutMs: SCROBBLE_PREFERENCES_TIMEOUT_MS
			}),
		// Kept in memory for the whole session: every navigation reads it.
		gcTime: Infinity
	});

export const getScrobblePreferencesQuery = () =>
	createQuery(() => ({
		...getScrobblePreferencesQueryOptions(authStore.user?.id),
		enabled: Boolean(authStore.user?.id)
	}));

// invalidate so the card and home/discover re-read the new primary source;
// the album and artist shelves follow it too
export const createUpdateScrobblePreferencesMutation = () =>
	createAppMutation(() => ({
		effects: () => [recommendationSourcesChanged(authStore.user?.id)],
		mutationFn: (vars: ScrobblePreferencesUpdate) =>
			// Partial by design (absent keeps stored values); the generated body
			// type over-states requiredness, so the patch is asserted at the call.
			api.v3.PUT(
				v3('/api/v3/me/scrobble-preferences'),
				vars as components['schemas']['ScrobblePreferencesUpdate']
			),
		onMutate: () => ({ userId: authStore.user?.id }),
		onSuccess: async (preferences, _vars, context) => {
			const userId = context.userId;
			if (!userId || authStore.user?.id !== userId) return;
			await setQueryDataWithPersister(ScrobblePreferencesQueryKeyFactory.get(userId), preferences);
		}
	}));

// kicks off a background build; playlist queries are invalidated by the
// personal_mix_refreshed event (queries/events/dispatcher.ts) when the build lands
export const createRefreshPersonalMixMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: () => api.v3.POST(v3('/api/v3/requests/personal-mix/refresh'))
	}));

interface PersonalMixApprovalVars {
	userId: string;
	userName: string | null;
}

const approvals = () => [
	personalMixApprovalsChanged(),
	pendingApprovalsChanged(authStore.user?.id)
];

function approvalErrorMessage(err: unknown, fallback: string): string {
	return err instanceof Error && err.message ? err.message : fallback;
}

export const createApprovePersonalMixMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: PersonalMixApprovalVars) =>
			api.v3.POST(
				v3('/api/v3/requests/personal-mix-approvals/{user_id}/approve', {
					path: { user_id: vars.userId }
				})
			),
		onSuccess: (_data, vars) => {
			toastStore.show({
				message: `Weekly Mix auto-request approved for ${vars.userName ?? 'user'}`,
				type: 'success'
			});
		},
		effects: approvals,
		onError: (err) =>
			toastStore.show({ message: approvalErrorMessage(err, 'Approve failed'), type: 'error' })
	}));

export const createRejectPersonalMixMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: PersonalMixApprovalVars) =>
			api.v3.POST(
				v3('/api/v3/requests/personal-mix-approvals/{user_id}/reject', {
					path: { user_id: vars.userId }
				})
			),
		onSuccess: (_data, vars) => {
			toastStore.show({
				message: `Weekly Mix auto-request rejected for ${vars.userName ?? 'user'}`,
				type: 'info'
			});
		},
		effects: approvals,
		onError: (err) =>
			toastStore.show({ message: approvalErrorMessage(err, 'Reject failed'), type: 'error' })
	}));

export const createRevokePersonalMixMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: PersonalMixApprovalVars) =>
			api.v3.POST(
				v3('/api/v3/requests/personal-mix-approvals/{user_id}/revoke', {
					path: { user_id: vars.userId }
				})
			),
		onSuccess: (data, vars) => {
			toastStore.show(
				data.success
					? {
							message: `Weekly Mix auto-request revoked for ${vars.userName ?? 'user'}`,
							type: 'info'
						}
					: { message: data.message, type: 'error' }
			);
		},
		effects: approvals,
		onError: (err) =>
			toastStore.show({ message: approvalErrorMessage(err, 'Revoke failed'), type: 'error' })
	}));
