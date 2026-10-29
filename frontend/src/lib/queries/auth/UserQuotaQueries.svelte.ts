import { createQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { createAppMutation, userQuotaChanged } from '$lib/queries/effects';

import { AuthQueryKeyFactory } from './AuthQueryKeyFactory';
import { AUTH_ENDPOINTS } from './endpoints';
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
		queryFn: ({ signal }) => api.v3.GET(AUTH_ENDPOINTS.userQuota(userId()), { signal })
	}));

export function saveUserQuota() {
	return createAppMutation(() => ({
		mutationFn: ({ userId, override }: { userId: string; override: UserQuotaOverride }) =>
			api.v3.PUT(AUTH_ENDPOINTS.userQuota(userId), override),
		effects: (_data, { userId }) => [userQuotaChanged(userId)]
	}));
}
