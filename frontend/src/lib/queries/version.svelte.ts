import { SHARED, key } from '$lib/queries/keys';
import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import type { components } from '$lib/api/v3/openapi';
import { STALE_SLOW } from '$lib/queries/staleTime';
import { createQuery } from '@tanstack/svelte-query';

export const VersionQueryKeyFactory = {
	info: () => key(SHARED, 'version', 'info'),
	updateCheck: () => key(SHARED, 'version', 'update-check'),
	releases: () => key(SHARED, 'version', 'releases')
};

export type GitHubRelease = components['schemas']['GitHubRelease'];

export const getVersionQuery = () =>
	createQuery(() => ({
		staleTime: STALE_SLOW,
		queryKey: VersionQueryKeyFactory.info(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/version'), { signal }),
		refetchOnWindowFocus: false,
		refetchOnMount: 'always'
	}));

export const getUpdateCheckQuery = () =>
	createQuery(() => ({
		staleTime: STALE_SLOW,
		queryKey: VersionQueryKeyFactory.updateCheck(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/version/check-update'), { signal }),
		refetchOnWindowFocus: false,
		refetchOnReconnect: false
	}));

export const getReleaseHistoryQuery = () =>
	createQuery(() => ({
		staleTime: STALE_SLOW,
		queryKey: VersionQueryKeyFactory.releases(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/version/releases'), { signal }),
		refetchOnWindowFocus: false,
		refetchOnReconnect: false
	}));
