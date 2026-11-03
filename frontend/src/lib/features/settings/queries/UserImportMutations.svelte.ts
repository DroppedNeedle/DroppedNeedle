import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { createAppMutation, userImportCandidatesChanged } from '$lib/queries/effects';
import type { ImportUsersResult, ImportUsersVars } from '$lib/features/auth/queries/types';

/**
 * Admin user-import mutation. On success it invalidates the
 * candidate list so `already_imported` flips; the admin user list is on a legacy
 * fetch, so the caller refreshes it via its own onImported callback.
 */
export const createImportUsersMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: ImportUsersVars) => api.v3.POST(v3('/api/v3/admin/import'), vars),
		effects: (_result: ImportUsersResult, vars: ImportUsersVars) => [
			userImportCandidatesChanged(authStore.user?.id, vars.provider)
		]
	}));
