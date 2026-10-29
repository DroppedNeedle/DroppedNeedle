import { api } from '$lib/api/client';
import { createMutation } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { AuthQueryKeyFactory } from './AuthQueryKeyFactory';
import { AUTH_ENDPOINTS } from './endpoints';
import type { ImportUsersResult, ImportUsersVars } from './types';

/**
 * Admin user-import mutation. On success it invalidates the
 * candidate list so `already_imported` flips; the admin user list is on a legacy
 * fetch, so the caller refreshes it via its own onImported callback.
 */
export const createImportUsersMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: ImportUsersVars) => api.v3.POST(AUTH_ENDPOINTS.adminImport, vars),
		onSuccess: (_result: ImportUsersResult, vars: ImportUsersVars) =>
			invalidateQueriesWithPersister({
				queryKey: AuthQueryKeyFactory.importCandidates(authStore.user?.id, vars.provider)
			})
	}));
