import { type QueryClient, createQuery, useQueryClient } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import type { components } from '$lib/api/v3/openapi';
import { createAppMutation } from '$lib/queries/effects';
import { SIGNED_OUT, key } from '$lib/queries/keys';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';

export type UpgradeState = components['schemas']['UpgradeStateBody'];
export type UpgradeProgress = components['schemas']['UpgradeProgressBody'];
export type UpgradeRun = components['schemas']['UpgradeRun'];
export type UpgradeCheck = components['schemas']['UpgradeCheck'];
export type UpgradeFailure = components['schemas']['UpgradeFailure'];
export type UpgradeResult = components['schemas']['UpgradeResult'];
export type UpgradeWarning = components['schemas']['UpgradeWarning'];
export type UpgradeAddress = components['schemas']['UpgradeAddress'];
export type UpgradeStep = components['schemas']['UpgradeStep'];
export type UpgradeSignIn = components['schemas']['UpgradeSignInBody'];

// The upgrade page belongs to no v3 user: its keys sit in the signed-out scope.
export const UpgradeQueryKeyFactory = {
	state: () => key(SIGNED_OUT, 'upgrade', 'state'),
	progress: () => key(SIGNED_OUT, 'upgrade', 'progress'),
	preflight: () => key(SIGNED_OUT, 'upgrade', 'preflight'),
	appReady: () => key(SIGNED_OUT, 'upgrade', 'app-ready')
};

const POLL_MS = 1_000;

/** Where the upgrade stands and whether this browser is signed in to it. */
export const getUpgradeStateQuery = () =>
	createQuery(() => ({
		queryKey: UpgradeQueryKeyFactory.state(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/upgrade/state'), { signal }),
		retry: false,
		refetchOnWindowFocus: false
	}));

/** The latest run, polled while it is under way. */
export const getUpgradeProgressQuery = (enabled: () => boolean) =>
	createQuery(() => ({
		queryKey: UpgradeQueryKeyFactory.progress(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/upgrade/progress'), { signal }),
		enabled: enabled(),
		retry: false,
		refetchOnWindowFocus: false,
		// A run keeps going while the page loses its connection, so polling
		// carries on through failed reads.
		refetchInterval: (query) => (query.state.data?.run?.running ? POLL_MS : false)
	}));

/**
 * The pre-flight: every check the upgrade runs before it moves anything. It
 * changes nothing, and runs again each time the screen opens, since a run in
 * between makes the server drop the one it held.
 */
export const getUpgradePreflightQuery = (enabled: () => boolean) =>
	createQuery(() => ({
		queryKey: UpgradeQueryKeyFactory.preflight(),
		queryFn: ({ signal }) => api.v3.POST(v3('/api/v3/upgrade/preflight'), undefined, { signal }),
		enabled: enabled(),
		staleTime: 0,
		retry: false,
		refetchOnMount: 'always',
		refetchOnWindowFocus: false,
		refetchOnReconnect: false
	}));

/**
 * After Continue: whether the app has taken over from the upgrade page. Its
 * first read answers once the app is up; until then it fails, and is asked
 * again.
 */
export const getUpgradeAppReadyQuery = (enabled: () => boolean) =>
	createQuery(() => ({
		queryKey: UpgradeQueryKeyFactory.appReady(),
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/auth/setup/status'), { signal, timeoutMs: 5_000 }),
		enabled: enabled(),
		retry: true,
		retryDelay: POLL_MS,
		refetchOnWindowFocus: false
	}));

function showProgress(client: QueryClient) {
	return (progress: UpgradeProgress) =>
		setQueryDataWithPersister(UpgradeQueryKeyFactory.progress(), () => progress, undefined, client);
}

export function createUpgradeSignInMutation() {
	const client = useQueryClient();
	return createAppMutation(() => ({
		mutationFn: (body: UpgradeSignIn) => api.v3.POST(v3('/api/v3/upgrade/sign-in'), body),
		onSuccess: (state: UpgradeState) =>
			setQueryDataWithPersister(UpgradeQueryKeyFactory.state(), () => state, undefined, client),
		effects: () => []
	}));
}

/** Upgrade, or finish an upgrade that did not finish. */
export function createUpgradeStartMutation() {
	const client = useQueryClient();
	return createAppMutation(() => ({
		mutationFn: (acknowledged: UpgradeWarning[]) =>
			api.v3.POST(v3('/api/v3/upgrade/start'), {
				acknowledged: acknowledged.map((warning) => ({
					feature: warning.feature,
					old_address: warning.old_address ?? null,
					new_address: warning.new_address ?? null
				}))
			}),
		onSuccess: showProgress(client),
		effects: () => []
	}));
}

export function createUpgradeFreshMutation() {
	const client = useQueryClient();
	return createAppMutation(() => ({
		mutationFn: () => api.v3.POST(v3('/api/v3/upgrade/fresh'), { confirm: true }),
		onSuccess: showProgress(client),
		effects: () => []
	}));
}

/** Go back to v2. */
export function createUpgradeUndoMutation() {
	const client = useQueryClient();
	return createAppMutation(() => ({
		mutationFn: () => api.v3.POST(v3('/api/v3/upgrade/undo')),
		onSuccess: showProgress(client),
		effects: () => []
	}));
}

/** Hand over to the app. */
export function createUpgradeContinueMutation() {
	return createAppMutation(() => ({
		mutationFn: () => api.v3.POST(v3('/api/v3/upgrade/continue')),
		effects: () => []
	}));
}

/** Forget a signed-in state the server no longer holds (the session ended). */
export function markUpgradeSignedOut(client: QueryClient) {
	return setQueryDataWithPersister(
		UpgradeQueryKeyFactory.state(),
		(state: UpgradeState | undefined) => (state ? { ...state, signed_in: false } : state),
		undefined,
		client
	);
}
