import { api } from '$lib/api/client';
import { REMOTE_ENDPOINTS } from '$lib/queries/remotes/endpoints';
import type { NavidromeFolderPreference } from '$lib/types';
import { createQuery } from '@tanstack/svelte-query';
import { NavidromeFolderQueryKeyFactory } from './NavidromeFolderQueryKeyFactory';
import { setNavidromeFolderScopeRevision } from '$lib/utils/navidromeLibraryCache';

export const getNavidromeFolderPreferenceQuery = (getUserId: () => string) =>
	createQuery(() => ({
		queryKey: NavidromeFolderQueryKeyFactory.preferences(getUserId()),
		queryFn: async ({ signal }) => {
			const view = await api.global.v3.GET(REMOTE_ENDPOINTS.folders(), { signal });
			// v3 has no revision token; the resolved selection itself scopes the
			// album-list cache, so a changed selection reads as a new scope.
			const result: NavidromeFolderPreference = {
				mode: view.mode === 'selected' ? 'selected' : 'all',
				selected_folder_ids: view.folder_ids,
				available_folders: view.available_folders,
				stale_folder_ids: view.stale_folder_ids,
				source_available: view.source_available,
				scope_revision: `${view.mode}:${[...view.folder_ids].sort().join(',')}`
			};
			setNavidromeFolderScopeRevision(getUserId(), result.scope_revision);
			return result;
		},
		enabled: Boolean(getUserId())
	}));
