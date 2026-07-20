// Queries/mutations for the fork's Prowlarr connection + qBittorrent client.
// Kept in their own module (not DownloadClientsQueries) to hold the upstream-merge
// surface down to new files; query keys extend the shared factory prefix locally.

import { createMutation, createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { API, CACHE_TTL } from '$lib/constants';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import type {
	ProwlarrConnectionSettings,
	ProwlarrTestResult,
	QbittorrentConnectionSettings,
	QbittorrentTestResult
} from '$lib/types';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';

const prowlarrKey = [...DownloadQueryKeyFactory.all, 'prowlarr'] as const;
const qbittorrentKey = [...DownloadQueryKeyFactory.all, 'qbittorrent'] as const;

const prowlarrOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: prowlarrKey,
		queryFn: ({ signal }) =>
			api.global.get<ProwlarrConnectionSettings>(API.prowlarr.settings(), { signal })
	});

export const getProwlarrConfigQuery = () => createQuery(() => prowlarrOptions());

export function saveProwlarrConfig() {
	return createMutation(() => ({
		mutationFn: (config: ProwlarrConnectionSettings) =>
			api.global.put<ProwlarrConnectionSettings>(API.prowlarr.settings(), config),
		onSuccess: async () => {
			await invalidateQueriesWithPersister({ queryKey: prowlarrKey });
			await invalidateQueriesWithPersister({ queryKey: HomeQueryKeyFactory.prefix });
		}
	}));
}

export function testProwlarr() {
	return createMutation(() => ({
		mutationFn: (config: ProwlarrConnectionSettings) =>
			api.global.post<ProwlarrTestResult>(API.prowlarr.test(), config)
	}));
}

const qbittorrentOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: qbittorrentKey,
		queryFn: ({ signal }) =>
			api.global.get<QbittorrentConnectionSettings>(API.downloadClients.qbittorrent(), {
				signal
			})
	});

export const getQbittorrentConfigQuery = () => createQuery(() => qbittorrentOptions());

export function saveQbittorrentConfig() {
	return createMutation(() => ({
		mutationFn: (config: QbittorrentConnectionSettings) =>
			api.global.put<QbittorrentConnectionSettings>(API.downloadClients.qbittorrent(), config),
		onSuccess: async () => {
			await invalidateQueriesWithPersister({ queryKey: qbittorrentKey });
			await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.clientStatus() });
			await invalidateQueriesWithPersister({ queryKey: HomeQueryKeyFactory.prefix });
		}
	}));
}

export function testQbittorrent() {
	return createMutation(() => ({
		mutationFn: (config: QbittorrentConnectionSettings) =>
			api.global.post<QbittorrentTestResult>(API.downloadClients.qbittorrentTest(), config)
	}));
}
