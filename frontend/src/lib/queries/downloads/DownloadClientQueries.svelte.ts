import { createMutation, createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { CACHE_TTL } from '$lib/constants';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { DOWNLOAD_SETTINGS_ENDPOINTS } from './endpoints';

export type DownloadClientConfig = components['schemas']['SlskdConnectionDto'];
export type DownloadClientStatusResponse = components['schemas']['SlskdStatusResponse'];
export type TestConnectionResult = components['schemas']['TestConnectionResponse'];

// v3 serves only the client half of the old status (configured, reachable,
// version, message); the mount half has no v3 endpoint yet, so mount fields
// stay undefined until the backend restores them. The settings UI keeps its
// mount guidance against this view, and the live probe maps onto the old
// client shape so the connection display is unchanged.
export interface SlskdMountView {
	ok: boolean;
	move_supported: boolean;
	reason: string;
	path: string | null;
}

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
		}
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
