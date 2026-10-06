import { api } from '$lib/api/client';
import { createQuery } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import { ConnectionsQueryKeyFactory } from './ConnectionsQueryKeyFactory';
import { CONNECTIONS_ENDPOINTS } from './endpoints';
import type { ConnectionsResponse } from './types';

/** The caller's linked accounts on every service, in one server read. A
 *  switched-off link is left out: consumers treat presence as linked. */
async function fetchConnections(signal?: AbortSignal): Promise<ConnectionsResponse> {
	const { connections } = await api.global.v3.GET(CONNECTIONS_ENDPOINTS.list(), { signal });
	return { connections: connections.filter((link) => link.enabled) };
}

export const getConnectionsQuery = () =>
	createQuery(() => ({
		queryKey: ConnectionsQueryKeyFactory.list(authStore.user?.id),
		queryFn: ({ signal }) => fetchConnections(signal)
	}));
