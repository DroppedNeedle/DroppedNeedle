import { SessionsQueryKeyFactory } from '$lib/queries/featureKeys';
import type { components } from '$lib/api/v3/openapi';

import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { createQuery, queryOptions } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { createAppMutation, sessionsChanged } from '$lib/queries/effects';
import { leaveSignedOut } from '$lib/utils/logout';

export type SessionView = components['schemas']['SessionView'];
export type SessionListResponse = components['schemas']['SessionListResponse'];

const SESSIONS_TIMEOUT_MS = 10_000;

export const getSessionsQueryOptions = (userId: string | undefined) =>
	queryOptions({
		queryKey: SessionsQueryKeyFactory.list(userId),
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/auth/sessions'), {
				signal,
				timeoutMs: SESSIONS_TIMEOUT_MS
			})
	});

export const getSessionsQuery = () =>
	createQuery(() => ({
		...getSessionsQueryOptions(authStore.user?.id),
		enabled: Boolean(authStore.user?.id)
	}));

export interface RevokeSessionVars {
	id: string;
	label: string;
}

function revokeErrorMessage(err: unknown): string {
	return err instanceof Error && err.message ? err.message : 'Could not revoke that session';
}

export const createRevokeSessionMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: RevokeSessionVars) =>
			api.v3.DELETE(v3('/api/v3/auth/sessions/{id}', { path: { id: vars.id } })),
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
		mutationFn: () => api.v3.POST(v3('/api/v3/auth/logout-all')),
		// Everything cached is cleared on the way out.
		effects: () => [],
		onSuccess: () => leaveSignedOut()
	}));
