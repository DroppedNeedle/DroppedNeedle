import { createMutation, createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { CACHE_TTL } from '$lib/constants';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { DOWNLOAD_SETTINGS_ENDPOINTS } from './endpoints';

export type DownloadClientConfig = components['schemas']['SlskdConnection'];
export type DownloadClientStatusResponse = components['schemas']['SlskdStatusResponse'];
export type TestConnectionResult = components['schemas']['TestConnectionResponse'];

// The live probe maps onto the old client shape so the connection display is
// unchanged. The mount half is filled by the server's periodic probe pass, so
// it is absent until the first pass after a restart.
export type SlskdMountView = components['schemas']['DownloadsMountView'];

export interface DownloadClientStatus {
	configured: boolean;
	reachable: boolean;
	version: string | null;
	message: string;
	client: { status: 'ok' | 'error'; version: string | null; message: string };
	mount?: SlskdMountView | null;
	mount_advisory?: string | null;
	slskd_downloads_dir?: string | null;
	effective_downloads_path?: string | null;
}

function toStatusView(status: DownloadClientStatusResponse): DownloadClientStatus {
	return {
		configured: status.configured,
		reachable: status.reachable,
		version: status.version ?? null,
		message: status.message,
		client: {
			status: status.reachable ? 'ok' : 'error',
			version: status.version ?? null,
			message: status.message
		},
		mount: status.mount ?? null,
		mount_advisory: status.mount_advisory ?? null,
		slskd_downloads_dir: status.slskd_downloads_dir ?? null,
		effective_downloads_path: status.effective_downloads_path ?? null
	};
}

const getDownloadClientConfigQueryOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: DownloadQueryKeyFactory.clientConfig(),
		queryFn: ({ signal }) =>
			api.global.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.slskdConfig(), { signal })
	});

export const getDownloadClientConfigQuery = () =>
	createQuery(() => getDownloadClientConfigQueryOptions());

const getDownloadClientStatusQueryOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: DownloadQueryKeyFactory.clientStatus(),
		queryFn: async ({ signal }) =>
			toStatusView(await api.global.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.slskdStatus(), { signal }))
	});

export const getDownloadClientStatusQuery = () =>
	createQuery(() => getDownloadClientStatusQueryOptions());

export function saveDownloadClientConfig() {
	return createMutation(() => ({
		mutationFn: (config: DownloadClientConfig) =>
			api.global.v3.PUT(DOWNLOAD_SETTINGS_ENDPOINTS.slskdConfig(), config),
		onSuccess: async () => {
			await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.clientConfig() });
			await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.clientStatus() });
			// Home reads integration_status.download_client; invalidate or its
			// "Configure Download Client" prompt lingers until the cache expires
			await invalidateQueriesWithPersister({ queryKey: HomeQueryKeyFactory.prefix });
		}
	}));
}

export function testDownloadClient() {
	return createMutation(() => ({
		mutationFn: (config: DownloadClientConfig) =>
			api.global.v3.POST(DOWNLOAD_SETTINGS_ENDPOINTS.slskdTest(), config)
	}));
}
