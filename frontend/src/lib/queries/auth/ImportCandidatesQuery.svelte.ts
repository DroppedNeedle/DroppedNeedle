import { api } from '$lib/api/client';
import { createQuery } from '@tanstack/svelte-query';
import { AuthQueryKeyFactory } from './AuthQueryKeyFactory';
import { AUTH_ENDPOINTS } from './endpoints';

/** Enumerated import candidates for a media service (admin-only). Uses the
 *  authenticated `api` client (the import flow runs post-session, unlike login).
 *  `provider`/`enabled` are getters so the query reacts to tab switches and only
 *  fires while the picker is open. */
export const getImportCandidatesQuery = (
	provider: () => 'jellyfin' | 'plex',
	enabled: () => boolean,
	userId: () => string | null | undefined
) =>
	createQuery(() => ({
		queryKey: AuthQueryKeyFactory.importCandidates(provider(), userId()),
		enabled: enabled(),
		queryFn: ({ signal }) =>
			api.v3.GET(
				provider() === 'plex' ? AUTH_ENDPOINTS.adminImportPlex : AUTH_ENDPOINTS.adminImportJellyfin,
				{ signal }
			)
	}));
