import { api } from '$lib/api/client';
import { toastStore } from '$lib/stores/toast';
import { authStore } from '$lib/stores/authStore.svelte';
import { SESSIONS_ENDPOINTS } from './endpoints';
import { createAppMutation, sessionsChanged } from '$lib/queries/effects';
import { leaveSignedOut } from '$lib/utils/logout';

export interface RevokeSessionVars {
	id: string;
	label: string;
}

function revokeErrorMessage(err: unknown): string {
	return err instanceof Error && err.message ? err.message : 'Could not revoke that session';
}

export const createRevokeSessionMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: RevokeSessionVars) => api.v3.DELETE(SESSIONS_ENDPOINTS.revoke(vars.id)),
		onMutate: () => ({ userId: authStore.user?.id }),
		effects: (_data, _vars, context) => {
			const userId = context.userId;
			return userId && authStore.user?.id === userId ? [sessionsChanged(userId)] : [];
		},
		onSuccess: (_data, vars) => {
			toastStore.show({ message: `Signed out ${vars.label}`, type: 'success' });
		},
		onError: (err) => toastStore.show({ message: revokeErrorMessage(err), type: 'error' })
	}));

/**
 * Signs the caller out of every browser and device, this one included, then
 * clears this browser's session state and opens the sign-in page. App
 * passwords are separate and keep working.
 */
export const createSignOutEverywhereMutation = () =>
	createAppMutation(() => ({
		mutationFn: () => api.v3.POST(SESSIONS_ENDPOINTS.logoutAll()),
		// Everything cached is cleared on the way out.
		effects: () => [],
		onSuccess: () => leaveSignedOut()
	}));
