import { createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { DOWNLOAD_SETTINGS_ENDPOINTS } from './endpoints';

const policySummaryOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.policySummary(),
		queryFn: ({ signal }) =>
			api.global.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.policySummary(), { signal })
	});

// Safe read-only acquisition-policy summary for ANY signed-in user (spec):
// the backend-composed contract sentence plus the source-mode label only.
export const getPolicySummaryQuery = () => createQuery(() => policySummaryOptions());
