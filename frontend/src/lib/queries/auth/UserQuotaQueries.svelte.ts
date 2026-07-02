import { createMutation, createQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';

import { AuthQueryKeyFactory } from './AuthQueryKeyFactory';

// Per-user request/storage quotas (CollectionManagement Feature C, admin-only).

export interface UserQuotaOverride {
	request_quota_count: number | null;
	request_quota_days: number | null;
	storage_quota_gb: number | null;
}

export interface UserQuotaResponse {
	user_id: string;
	override: UserQuotaOverride;
	effective_request_quota_count: number;
	effective_request_quota_days: number;
	effective_storage_quota_gb: number;
	requests_in_window: number;
	storage_bytes: number;
	exempt: boolean;
}

const quotaUrl = (userId: string) => `/api/v1/auth/admin/users/${encodeURIComponent(userId)}/quota`;

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
		queryFn: ({ signal }) => api.get<UserQuotaResponse>(quotaUrl(userId()), { signal })
	}));

export function saveUserQuota() {
	return createMutation(() => ({
		mutationFn: ({ userId, override }: { userId: string; override: UserQuotaOverride }) =>
			api.put<UserQuotaResponse>(quotaUrl(userId), override),
		onSuccess: (_data, { userId }) =>
			invalidateQueriesWithPersister({ queryKey: userQuotaKey(userId) })
	}));
}
