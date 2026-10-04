import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { createMutation } from '@tanstack/svelte-query';
import { toastStore } from '$lib/stores/toast';
import { authStore } from '$lib/stores/authStore.svelte';
import { invalidateQueriesWithPersister, setQueryDataWithPersister } from '../QueryClient';
import { ScrobblePreferencesQueryKeyFactory } from './ScrobblePreferencesQueryKeyFactory';
import { SCROBBLE_PREFERENCES_ENDPOINTS } from './endpoints';
import { notifyPendingApprovalCountChanged } from '$lib/utils/requestsApi';
import type { ScrobblePreferencesUpdate } from './types';
import { isMusicSource, musicSourceStore } from '$lib/stores/musicSource';

// invalidate so the card + (Phase 5) home/discover re-read the new primary source
export const createUpdateScrobblePreferencesMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: ScrobblePreferencesUpdate) =>
			// Partial by design (absent keeps stored values); the generated body
			// type over-states requiredness, so the patch is asserted at the call.
			api.global.v3.PUT(
				SCROBBLE_PREFERENCES_ENDPOINTS.update(),
				vars as components['schemas']['ScrobblePreferencesUpdate']
			),
		onMutate: () => ({ userId: authStore.user?.id }),
		onSuccess: async (preferences, _vars, context) => {
			const userId = context.userId;
			if (!userId || authStore.user?.id !== userId) return;
			if (isMusicSource(preferences.primary_music_source)) {
				musicSourceStore.setSource(preferences.primary_music_source);
			}
			await setQueryDataWithPersister(ScrobblePreferencesQueryKeyFactory.get(userId), preferences);
		}
	}));

// kicks off a background build; playlist queries are invalidated by the
// personal_mix_refreshed SSE handler (FollowingEvents) when the build lands
export const createRefreshPersonalMixMutation = () =>
	createMutation(() => ({
		mutationFn: () => api.global.v3.POST(SCROBBLE_PREFERENCES_ENDPOINTS.refreshPersonalMixV3())
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
			api.global.v3.POST(SCROBBLE_PREFERENCES_ENDPOINTS.approvePersonalMix(vars.userId)),
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
			api.global.v3.POST(SCROBBLE_PREFERENCES_ENDPOINTS.rejectPersonalMix(vars.userId)),
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
