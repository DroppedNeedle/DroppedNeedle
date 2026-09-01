import { browser } from '$app/environment';
import { ApiError, api } from '$lib/api/client';
import { queryClient } from '$lib/queries/QueryClient';
import { API, AUTH_FREE_PATHS } from '$lib/constants';
import { toAuthUser, type AuthSessionUser } from '$lib/queries/auth/types';
import { getScrobblePreferencesQueryOptions } from '$lib/queries/scrobble-preferences/ScrobblePreferencesQuery.svelte';
import { DEFAULT_SOURCE, isMusicSource, musicSourceStore } from '$lib/stores/musicSource';
import { authStore, LAST_USER_ID_KEY } from '$lib/stores/authStore.svelte';
import { clearUserSessionState } from '$lib/utils/userSessionCleanup';
import { withBasePath, withoutBasePath } from '$lib/utils/basePath';
import { error, redirect } from '@sveltejs/kit';
import type { LayoutLoad } from './$types';

export const ssr = false;
export const prerender = false;

const BOOTSTRAP_TIMEOUT_MS = 10_000;
const BUSY_MESSAGE = 'The server is busy. Your session is safe - try again shortly.';

export const load: LayoutLoad = async ({ url }) => {
	const path = withoutBasePath(url.pathname);
	const isAuthFree = AUTH_FREE_PATHS.some((p) => path.startsWith(p));

	let setupRequired = authStore.setupRequired;
	if (!authStore.initialized) {
		try {
			const status = await api.global.get<{ required: boolean }>(API.auth.setupStatus(), {
				timeoutMs: BOOTSTRAP_TIMEOUT_MS
			});
			setupRequired = status.required;
			authStore.setSetupRequired(setupRequired);
		} catch {
			throw error(503, BUSY_MESSAGE);
		}

		try {
			const user = await api.global.get<AuthSessionUser>(API.auth.me(), {
				timeoutMs: BOOTSTRAP_TIMEOUT_MS
			});
			authStore.setUser(toAuthUser(user));
		} catch (cause) {
			if (cause instanceof ApiError && cause.status === 401) {
				await clearUserSessionState().catch(() => undefined);
			} else {
				throw error(503, BUSY_MESSAGE);
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

	// the primary source is user-specific; connection defaults are global
	let primarySource = DEFAULT_SOURCE;
	if (authStore.isAuthenticated) {
		try {
			const data = await queryClient.ensureQueryData(
				getScrobblePreferencesQueryOptions(authStore.user?.id)
			);
			if (isMusicSource(data.primary_music_source)) {
				primarySource = data.primary_music_source;
				musicSourceStore.setSource(primarySource);
			}
		} catch {
			primarySource = DEFAULT_SOURCE;
		}
	}

	return { primarySource, user: authStore.user };
};
