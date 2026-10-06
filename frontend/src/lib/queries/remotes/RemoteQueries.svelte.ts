import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';

import {
	REMOTE_ENDPOINTS,
	type RemoteMixParams,
	type RemotePageParams,
	type RemoteRandomParams
} from './endpoints';
import { RemoteQueryKeyFactory } from './RemoteQueryKeyFactory';
import { remoteApi } from './remoteApi';
import type { RemoteSource } from './types';

type Source = Getter<RemoteSource>;
type Enabled = Getter<boolean>;

const userId = () => authStore.user?.id;
const authed = (getEnabled: Enabled) => getEnabled() && !!userId();

// Each call site passes its own load closure so the typed client infers
// the response from that call's registry URL; the reads the typed client
// cannot infer yet go through remoteApi (see there).
function query<T>(
	build: () => {
		key: readonly unknown[];
		load: (signal: AbortSignal) => Promise<T>;
		getEnabled: Enabled;
		extraGate?: boolean;
	}
) {
	return createQuery(() => {
		const { key, load, getEnabled, extraGate = true } = build();
		return {
			queryKey: key,
			queryFn: ({ signal }: { signal: AbortSignal }) => load(signal),
			enabled: authed(getEnabled) && extraGate
		};
	});
}

export const getRemoteHubQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.hub(userId(), getSource()),
		load: (signal: AbortSignal) => api.global.v3.GET(REMOTE_ENDPOINTS.hub(getSource()), { signal }),
		getEnabled
	}));

export const getRemoteConnectionQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.connection(userId(), getSource()),
		load: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.connection(getSource()), { signal }),
		getEnabled
	}));

export const getRemoteArtistIndexQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.artistIndex(userId(), getSource()),
		load: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.artistIndex(getSource()), { signal }),
		getEnabled
	}));

export const getRemoteFavoritesQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.favorites(userId(), getSource()),
		load: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.favorites(getSource()), { signal }),
		getEnabled
	}));

export const getRemoteHistoryQuery = (
	getSource: Source,
	getParams: Getter<RemotePageParams> = () => ({}),
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.history(userId(), getSource(), getParams()),
		load: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.history(getSource(), getParams()), { signal }),
		getEnabled
	}));

export const getRemoteStatsQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.stats(userId(), getSource()),
		load: (signal: AbortSignal) => remoteApi.stats(getSource(), signal),
		getEnabled
	}));

export const getRemoteInfoArtistQuery = (
	getSource: Source,
	getId: Getter<string>,
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.infoArtist(userId(), getSource(), getId()),
		load: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.infoArtist(getSource(), getId()), { signal }),
		getEnabled,
		extraGate: getId().length > 0
	}));

export const getRemoteMixQuery = (
	getSource: Source,
	getId: Getter<string>,
	getParams: Getter<RemoteMixParams> = () => ({}),
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.mix(userId(), getSource(), getId(), getParams()),
		load: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.mix(getSource(), getId(), getParams()), { signal }),
		getEnabled,
		extraGate: getId().length > 0
	}));

export const getRemoteSimilarQuery = (
	getSource: Source,
	getId: Getter<string>,
	getParams: Getter<RemotePageParams> = () => ({}),
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.similar(userId(), getSource(), getId(), getParams()),
		load: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.similar(getSource(), getId(), getParams()), { signal }),
		getEnabled,
		extraGate: getId().length > 0
	}));

export const getRemoteTopQuery = (
	getSource: Source,
	getArtist: Getter<string>,
	getLimit: Getter<number | undefined> = () => undefined,
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.top(userId(), getSource(), getArtist(), getLimit()),
		load: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.top(getSource(), getArtist(), getLimit()), { signal }),
		getEnabled,
		extraGate: getArtist().length > 0
	}));

export const getRemoteRandomQuery = (
	getSource: Source,
	getParams: Getter<RemoteRandomParams> = () => ({}),
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.random(userId(), getSource(), getParams()),
		load: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.random(getSource(), getParams()), { signal }),
		getEnabled
	}));

export const getRemoteDiscoveryQuery = (
	getSource: Source,
	getCount: Getter<number | undefined> = () => undefined,
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.discovery(userId(), getSource(), getCount()),
		load: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.discovery(getSource(), getCount()), { signal }),
		getEnabled
	}));

export const getRemoteFoldersQuery = (getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.folders(userId()),
		load: (signal: AbortSignal) => api.global.v3.GET(REMOTE_ENDPOINTS.folders(), { signal }),
		getEnabled
	}));
