import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { createAppMutation, userImportCandidatesChanged } from '../effects';
import { AUTH_ENDPOINTS } from './endpoints';
import type { ImportUsersResult, ImportUsersVars } from './types';

/**
 * Admin user-import mutation. On success it invalidates the
 * candidate list so `already_imported` flips; the admin user list is on a legacy
 * fetch, so the caller refreshes it via its own onImported callback.
 */
export const createImportUsersMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: ImportUsersVars) => api.v3.POST(AUTH_ENDPOINTS.adminImport, vars),
		effects: (_result: ImportUsersResult, vars: ImportUsersVars) => [
			userImportCandidatesChanged(authStore.user?.id, vars.provider)
		]
	}));
