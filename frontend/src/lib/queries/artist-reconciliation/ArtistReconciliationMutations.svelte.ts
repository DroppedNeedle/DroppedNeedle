import { api } from '$lib/api/client';
import { toastStore } from '$lib/stores/toast';

import { artistDuplicatesChanged, createAppMutation } from '$lib/queries/effects';
import { ARTIST_RECONCILIATION_ENDPOINTS } from './endpoints';

export const dismissArtistDuplicateGroup = () =>
	createAppMutation(() => ({
		mutationFn: (input: { groupId: string; expectedMemberRevisions: Record<string, number> }) =>
			api.v3.POST(ARTIST_RECONCILIATION_ENDPOINTS.dismiss(input.groupId), {
				expected_member_revisions: input.expectedMemberRevisions
			}),
		effects: () => [artistDuplicatesChanged()],
		onSuccess: () => {
			toastStore.show({ message: 'Artist records marked as distinct', type: 'success' });
		},
		onError: () =>
			toastStore.show({
				message: 'The artist records changed; review the group again',
				type: 'error'
			})
	}));
