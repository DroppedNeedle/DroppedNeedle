import { api } from '$lib/api/client';
import { createMutation } from '@tanstack/svelte-query';
import { toastStore } from '$lib/stores/toast';
import { authStore } from '$lib/stores/authStore.svelte';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { SessionsQueryKeyFactory } from './SessionsQueryKeyFactory';
import { SESSIONS_ENDPOINTS } from './endpoints';

export interface RevokeSessionVars {
	id: string;
	label: string;
}

function revokeErrorMessage(err: unknown): string {
	return err instanceof Error && err.message ? err.message : 'Could not revoke that session';
}

export const createRevokeSessionMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: RevokeSessionVars) =>
			api.global.v3.DELETE(SESSIONS_ENDPOINTS.revoke(vars.id)),
		onMutate: () => ({ userId: authStore.user?.id }),
		onSuccess: async (_data, vars, context) => {
			const userId = context.userId;
			if (userId && authStore.user?.id === userId) {
				await invalidateQueriesWithPersister({
					queryKey: SessionsQueryKeyFactory.list(userId)
				});
			}
			toastStore.show({ message: `Signed out ${vars.label}`, type: 'success' });
		},
		onError: (err) => toastStore.show({ message: revokeErrorMessage(err), type: 'error' })
	}));
