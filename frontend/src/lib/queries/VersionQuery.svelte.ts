import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import type { components } from '$lib/api/v3/openapi';
import { CACHE_TTL } from '$lib/constants';
import { createQuery } from '@tanstack/svelte-query';
import { VersionQueryKeyFactory } from './VersionQueryKeyFactory';

export type GitHubRelease = components['schemas']['GitHubRelease'];

export const VERSION_ENDPOINTS = {
	info: () => v3('/api/v3/version'),
	checkUpdate: () => v3('/api/v3/version/check-update'),
	releases: () => v3('/api/v3/version/releases')
} as const;

export const getVersionQuery = () =>
	createQuery(() => ({
		staleTime: CACHE_TTL.VERSION_INFO,
		queryKey: VersionQueryKeyFactory.info(),
		queryFn: ({ signal }) => api.global.v3.GET(VERSION_ENDPOINTS.info(), { signal }),
		refetchOnWindowFocus: false,
		refetchOnMount: 'always'
	}));

export const getUpdateCheckQuery = () =>
	createQuery(() => ({
		staleTime: CACHE_TTL.UPDATE_CHECK,
		queryKey: VersionQueryKeyFactory.updateCheck(),
		queryFn: ({ signal }) => api.global.v3.GET(VERSION_ENDPOINTS.checkUpdate(), { signal }),
		refetchOnWindowFocus: false,
		refetchOnReconnect: false
	}));

export const getReleaseHistoryQuery = () =>
	createQuery(() => ({
		staleTime: CACHE_TTL.RELEASE_HISTORY,
		queryKey: VersionQueryKeyFactory.releases(),
		queryFn: ({ signal }) => api.global.v3.GET(VERSION_ENDPOINTS.releases(), { signal }),
		refetchOnWindowFocus: false,
		refetchOnReconnect: false
	}));
