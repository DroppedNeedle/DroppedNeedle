import { createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { API, CACHE_TTL } from '$lib/constants';
import type { PolicySummary } from '$lib/types';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';

const policySummaryOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: DownloadQueryKeyFactory.policySummary(),
		queryFn: ({ signal }) =>
			api.global.get<PolicySummary>(API.downloadClients.policySummary(), { signal })
	});

// Safe read-only acquisition-policy summary for ANY signed-in user (spec):
// the backend-composed contract sentence plus the source-mode label only.
export const getPolicySummaryQuery = () => createQuery(() => policySummaryOptions());
