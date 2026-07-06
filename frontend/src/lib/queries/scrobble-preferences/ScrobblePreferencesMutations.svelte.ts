import { api } from '$lib/api/client';
import { createMutation } from '@tanstack/svelte-query';
import { toastStore } from '$lib/stores/toast';
import { authStore } from '$lib/stores/authStore.svelte';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { ScrobblePreferencesQueryKeyFactory } from './ScrobblePreferencesQueryKeyFactory';
import { SCROBBLE_PREFERENCES_ENDPOINTS } from './endpoints';
import { notifyPendingApprovalCountChanged } from '$lib/utils/requestsApi';
import type {
	ApprovalActionResponse,
	PersonalMixRefreshResponse,
	ScrobblePreferences,
	ScrobblePreferencesUpdate
} from './types';

// invalidate so the card + (Phase 5) home/discover re-read the new primary source
export const createUpdateScrobblePreferencesMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: ScrobblePreferencesUpdate) =>
			api.global.put<ScrobblePreferences>(SCROBBLE_PREFERENCES_ENDPOINTS.update, vars),
		onSuccess: () =>
			invalidateQueriesWithPersister({
				queryKey: ScrobblePreferencesQueryKeyFactory.get(authStore.user?.id)
			})
	}));

// kicks off a background build; playlist queries are invalidated by the
// personal_mix_refreshed SSE handler (FollowingEvents) when the build lands
export const createRefreshPersonalMixMutation = () =>
	createMutation(() => ({
		mutationFn: () =>
			api.global.post<PersonalMixRefreshResponse>(
				SCROBBLE_PREFERENCES_ENDPOINTS.refreshPersonalMix,
				{}
			)
	}));

interface PersonalMixApprovalVars {
	userId: string;
	userName: string | null;
}

function invalidatePersonalMixApprovals(): Promise<void> {
	return invalidateQueriesWithPersister({
		queryKey: ScrobblePreferencesQueryKeyFactory.personalMixApprovals()
	});
}

function approvalErrorMessage(err: unknown, fallback: string): string {
	return err instanceof Error && err.message ? err.message : fallback;
}

export const createApprovePersonalMixMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: PersonalMixApprovalVars) =>
			api.global.post<ApprovalActionResponse>(
				SCROBBLE_PREFERENCES_ENDPOINTS.approvePersonalMix(vars.userId)
			),
		onSuccess: async (_data, vars) => {
			toastStore.show({
				message: `Weekly Mix auto-request approved for ${vars.userName ?? 'user'}`,
				type: 'success'
			});
			await invalidatePersonalMixApprovals();
			notifyPendingApprovalCountChanged();
		},
		onError: (err) =>
			toastStore.show({ message: approvalErrorMessage(err, 'Approve failed'), type: 'error' })
	}));

export const createRejectPersonalMixMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: PersonalMixApprovalVars) =>
			api.global.post<ApprovalActionResponse>(
				SCROBBLE_PREFERENCES_ENDPOINTS.rejectPersonalMix(vars.userId)
			),
		onSuccess: async (_data, vars) => {
			toastStore.show({
				message: `Weekly Mix auto-request rejected for ${vars.userName ?? 'user'}`,
				type: 'info'
			});
			await invalidatePersonalMixApprovals();
			notifyPendingApprovalCountChanged();
		},
		onError: (err) =>
			toastStore.show({ message: approvalErrorMessage(err, 'Reject failed'), type: 'error' })
	}));
