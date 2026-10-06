import { createMutation } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { toastStore } from '$lib/stores/toast';

import { invalidateArtistReconciliation } from './ArtistReconciliationInvalidation';
import { ARTIST_RECONCILIATION_ENDPOINTS } from './endpoints';

export const dismissArtistDuplicateGroup = () =>
	createMutation(() => ({
		mutationFn: (input: { groupId: string; expectedMemberRevisions: Record<string, number> }) =>
			api.global.v3.POST(ARTIST_RECONCILIATION_ENDPOINTS.dismiss(input.groupId), {
				expected_member_revisions: input.expectedMemberRevisions
			}),
		onSuccess: async () => {
			await invalidateArtistReconciliation();
			toastStore.show({ message: 'Artist records marked as distinct', type: 'success' });
		},
		onError: () =>
			toastStore.show({
				message: 'The artist records changed; review the group again',
				type: 'error'
			})
	}));
