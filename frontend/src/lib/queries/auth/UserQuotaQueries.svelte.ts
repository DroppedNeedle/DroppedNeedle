import { v3 } from '$lib/api/v3/endpoint';
import { createQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { createAppMutation, userQuotaChanged } from '$lib/queries/effects';

import { AuthQueryKeyFactory } from './AuthQueryKeyFactory';
import { STALE_LIVE } from '$lib/queries/staleTime';

// Per-user request/storage quotas (CollectionManagement Feature C, admin-only).
// v3 names the override block `quota_override` (v1 `override`).

export type UserQuotaOverride = components['schemas']['QuotaOverrideBody'];

export type UserQuotaResponse = components['schemas']['QuotaResponse'];

export const getUserQuotaQuery = (userId: () => string, enabled: () => boolean) =>
	createQuery(() => ({
		queryKey: AuthQueryKeyFactory.userQuota(userId()),
		enabled: enabled(),
		// usage numbers must be live: without this the 1-min default staleTime +
		// the IndexedDB persister serve yesterday's counts on a soft refresh
		staleTime: STALE_LIVE,
		refetchOnMount: 'always' as const,
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/admin/users/{id}/quota', { path: { id: userId() } }), { signal })
	}));

export function createSaveUserQuotaMutation() {
	return createAppMutation(() => ({
		mutationFn: ({ userId, override }: { userId: string; override: UserQuotaOverride }) =>
			api.v3.PUT(v3('/api/v3/admin/users/{id}/quota', { path: { id: userId } }), override),
		effects: (_data, { userId }) => [userQuotaChanged(userId)]
	}));
}
