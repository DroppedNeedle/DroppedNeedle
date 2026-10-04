import { createMutation, createQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';

import { AuthQueryKeyFactory } from './AuthQueryKeyFactory';
import { AUTH_ENDPOINTS } from './endpoints';

// Per-user request/storage quotas (CollectionManagement Feature C, admin-only).
// v3 names the override block `quota_override` (v1 `override`).

export type UserQuotaOverride = components['schemas']['QuotaOverrideBody'];

export type UserQuotaResponse = components['schemas']['QuotaResponse'];

export const userQuotaKey = (userId: string) =>
	[...AuthQueryKeyFactory.prefix, 'user-quota', userId] as const;

export const getUserQuotaQuery = (userId: () => string, enabled: () => boolean) =>
	createQuery(() => ({
		queryKey: userQuotaKey(userId()),
		enabled: enabled(),
		// usage numbers must be live: without this the 1-min default staleTime +
		// the IndexedDB persister serve yesterday's counts on a soft refresh
		staleTime: 0,
		refetchOnMount: 'always' as const,
		queryFn: ({ signal }) => api.v3.GET(AUTH_ENDPOINTS.userQuota(userId()), { signal })
	}));

export function saveUserQuota() {
	return createMutation(() => ({
		mutationFn: ({ userId, override }: { userId: string; override: UserQuotaOverride }) =>
			api.v3.PUT(AUTH_ENDPOINTS.userQuota(userId), override),
		onSuccess: (_data, { userId }) =>
			invalidateQueriesWithPersister({ queryKey: userQuotaKey(userId) })
	}));
}
