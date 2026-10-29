import { browser } from '$app/environment';
import { ApiError, TransportError, api } from '$lib/api/client';
import { queryClient } from '$lib/queries/QueryClient';
import { AUTH_FREE_PATHS } from '$lib/constants';
import { AUTH_ENDPOINTS } from '$lib/queries/auth/endpoints';
import { PROFILE_ENDPOINTS } from '$lib/queries/profile/endpoints';
import { toAuthUser } from '$lib/queries/auth/types';
import { getScrobblePreferencesQueryOptions } from '$lib/queries/scrobble-preferences/ScrobblePreferencesQuery.svelte';
import { DEFAULT_SOURCE, isMusicSource } from '$lib/stores/musicSource';
import { authStore, LAST_USER_ID_KEY } from '$lib/stores/authStore.svelte';
import { clearUserSessionState } from '$lib/utils/userSessionCleanup';
import { withBasePath, withoutBasePath } from '$lib/utils/basePath';
import { SERVER_BUSY_ERROR, UNREACHABLE_ERROR } from '$lib/utils/pageErrors';
// Applies the stored theme before the app shell and its CSS load.
import '$lib/stores/theme.svelte';
import { error, redirect } from '@sveltejs/kit';
import type { LayoutLoad } from './$types';

export const ssr = false;
export const prerender = false;

const BOOTSTRAP_TIMEOUT_MS = 10_000;
// Waits before trying again a startup request that never reached the server.
const BOOTSTRAP_RETRY_DELAYS_MS = [250, 1_000];

// A dropped connection or a network change fails a request before the server
// sees it; that is worth another try. A timeout already waited long enough.
async function retryUnreached<T>(request: () => Promise<T>): Promise<T> {
	for (const delay of BOOTSTRAP_RETRY_DELAYS_MS) {
		try {
			return await request();
		} catch (cause) {
			if (!(cause instanceof TransportError) || cause.code === 'TRANSPORT_TIMEOUT') throw cause;
			await new Promise((resolve) => setTimeout(resolve, delay));
		}
	}
	return request();
}

// Busy only when the server said so; a request that never arrived is not.
function startupError(cause: unknown): never {
	if (cause instanceof TransportError) error(503, UNREACHABLE_ERROR);
	if (cause instanceof ApiError && cause.status === 503 && cause.code === 'SERVER_BUSY') {
		error(503, SERVER_BUSY_ERROR);
	}
	error(500, cause instanceof ApiError ? cause.message : 'Internal Error');
}

export const load: LayoutLoad = async ({ url }) => {
	const path = withoutBasePath(url.pathname);
	const isAuthFree = AUTH_FREE_PATHS.some((p) => path.startsWith(p));

	let setupRequired = authStore.setupRequired;
	if (!authStore.initialized) {
		try {
			const status = await retryUnreached(() =>
				api.global.v3.GET(AUTH_ENDPOINTS.setupStatus, { timeoutMs: BOOTSTRAP_TIMEOUT_MS })
			);
			setupRequired = status.setup_required;
			authStore.setSetupRequired(setupRequired);
		} catch (cause) {
			throw startupError(cause);
		}

		try {
			const user = await retryUnreached(() =>
				api.global.v3.GET(PROFILE_ENDPOINTS.get(), { timeoutMs: BOOTSTRAP_TIMEOUT_MS })
			);
			authStore.setUser(toAuthUser(user));
		} catch (cause) {
			if (cause instanceof ApiError && cause.status === 401) {
				await clearUserSessionState().catch(() => undefined);
			} else {
				throw startupError(cause);
			}
		}
		authStore.markInitialized();
	}

	if (setupRequired && !isAuthFree) {
		throw redirect(302, withBasePath('/setup'));
	}
	if (!setupRequired && path.startsWith('/setup')) {
		throw redirect(302, withBasePath(authStore.isAuthenticated ? '/' : '/login'));
	}

	// initialized stays true after in-app login; reset persisted caches on account switches
	if (browser && authStore.user) {
		const lastId = localStorage.getItem(LAST_USER_ID_KEY);
		if (lastId && lastId !== authStore.user.id) {
			await clearUserSessionState({ clearAuth: false }).catch(() => undefined);
		}
		localStorage.setItem(LAST_USER_ID_KEY, authStore.user.id);
	}

	if (!setupRequired && !isAuthFree && !authStore.isAuthenticated) {
		throw redirect(302, withBasePath('/login'));
	}

	// The primary source is the user's scrobble preference. Once it is cached a
	// navigation reads it and refreshes it in the background.
	let primarySource = DEFAULT_SOURCE;
	if (authStore.isAuthenticated) {
		const options = getScrobblePreferencesQueryOptions(authStore.user?.id);
		try {
			const cached = queryClient.getQueryData(options.queryKey);
			if (cached) void queryClient.prefetchQuery(options);
			const data = cached ?? (await queryClient.ensureQueryData(options));
			if (isMusicSource(data.primary_music_source)) primarySource = data.primary_music_source;
		} catch {
			primarySource = DEFAULT_SOURCE;
		}
	}

	return { primarySource, user: authStore.user };
};
