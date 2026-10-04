import { api } from '$lib/api/client';
import { createQuery } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import { REMOTE_ENDPOINTS } from '$lib/queries/remotes/endpoints';
import { SPOTIFY_ENDPOINTS } from '$lib/queries/spotify/endpoints';
import { ConnectionsQueryKeyFactory } from './ConnectionsQueryKeyFactory';
import { CONNECTIONS_ENDPOINTS } from './endpoints';
import type { ConnectionStatus, ConnectionsResponse } from './types';

/** The caller's linked accounts, aggregated from the per-service v3 reads
 *  (v3 has no aggregate list). Reads settle independently: an unlinked
 *  service answers 404/400, which drops it from the list rather than
 *  failing the query. Media servers count only with an own (`linked`)
 *  credential - a shared admin credential makes the server usable but is
 *  not the caller's link. */
async function fetchConnections(signal?: AbortSignal): Promise<ConnectionsResponse> {
	// Written out (not mapped): the tuple overload keeps each read's
	// inferred response type at its own position.
	const settled = await Promise.allSettled([
		api.global.v3.GET(REMOTE_ENDPOINTS.connection('navidrome'), { signal }),
		api.global.v3.GET(REMOTE_ENDPOINTS.connection('jellyfin'), { signal }),
		api.global.v3.GET(REMOTE_ENDPOINTS.connection('plex'), { signal }),
		api.global.v3.GET(CONNECTIONS_ENDPOINTS.listenbrainz(), { signal }),
		api.global.v3.GET(CONNECTIONS_ENDPOINTS.lastfm(), { signal }),
		api.global.v3.GET(SPOTIFY_ENDPOINTS.playlists(), { signal })
	]);

	const connections: ConnectionStatus[] = [];
	const [navidrome, jellyfin, plex, listenbrainz, lastfm, spotify] = settled;

	for (const [service, result] of [
		['navidrome', navidrome],
		['jellyfin', jellyfin],
		['plex', plex]
	] as const) {
		if (result.status === 'fulfilled') {
			const status = result.value;
			if (status.connected && status.account_mode === 'linked') {
				connections.push({ service, enabled: true, username: status.account_label });
			}
		}
	}
	if (listenbrainz.status === 'fulfilled' && listenbrainz.value.enabled !== false) {
		connections.push({
			service: 'listenbrainz',
			enabled: true,
			username: listenbrainz.value.username ?? ''
		});
	}
	if (lastfm.status === 'fulfilled' && lastfm.value.linked) {
		connections.push({
			service: 'lastfm',
			enabled: true,
			username: lastfm.value.username ?? ''
		});
	}
	if (spotify.status === 'fulfilled') {
		connections.push({ service: 'spotify', enabled: true, username: '' });
	}
	return { connections };
}

export const getConnectionsQuery = () =>
	createQuery(() => ({
		queryKey: ConnectionsQueryKeyFactory.list(authStore.user?.id),
		queryFn: ({ signal }) => fetchConnections(signal)
	}));
