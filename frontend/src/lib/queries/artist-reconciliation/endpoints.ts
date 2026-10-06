import { v3 } from '$lib/api/v3/endpoint';
import type { V3Query } from '$lib/api/v3/endpoint';

export const ARTIST_RECONCILIATION_ENDPOINTS = {
	progress: () => v3('/api/v3/library/artists/reconciliation'),
	groups: (query: V3Query) => v3('/api/v3/library/artists/duplicate-groups', { query }),
	group: (groupId: string) =>
		v3('/api/v3/library/artists/duplicate-groups/{group_id}', { path: { group_id: groupId } }),
	dismiss: (groupId: string) =>
		v3('/api/v3/library/artists/duplicate-groups/{group_id}/dismiss', {
			path: { group_id: groupId }
		})
} as const;
