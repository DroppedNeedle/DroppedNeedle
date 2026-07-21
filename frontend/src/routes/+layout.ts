import { browser } from '$app/environment';
import { ApiError, api } from '$lib/api/client';
import { API, AUTH_FREE_PATHS } from '$lib/constants';
import { resetQueryCacheForUserSwitch } from '$lib/queries/QueryClient';
import { DEFAULT_SOURCE, isMusicSource, musicSourceStore } from '$lib/stores/musicSource';
import { scrobbleManager } from '$lib/stores/scrobble.svelte';
import { authStore, LAST_USER_ID_KEY } from '$lib/stores/authStore.svelte';
import { clearUserScopedLocalCaches } from '$lib/utils/userScopedCaches';
import { error, redirect } from '@sveltejs/kit';
import type { LayoutLoad } from './$types';

export const ssr = false;
export const prerender = false;

const BOOTSTRAP_TIMEOUT_MS = 10_000;
const BUSY_MESSAGE = 'The server is busy. Your session is safe - try again shortly.';

export const load: LayoutLoad = async ({ url }) => {
	const path = url.pathname;
	const isAuthFree = AUTH_FREE_PATHS.some((p) => path.startsWith(p));

	let setupRequired = false;
	try {
		const status = await api.global.get<{ required: boolean }>(API.auth.setupStatus(), {
			timeoutMs: BOOTSTRAP_TIMEOUT_MS
		});
		setupRequired = status.required;
	} catch {
		throw error(503, BUSY_MESSAGE);
	}

	if (setupRequired && !isAuthFree) {
		throw redirect(302, '/setup');
	}

	if (!authStore.initialized) {
		try {
			const user = await api.global.get<{
				id: string;
				display_name: string;
				role: string;
				email: string | null;
				avatar_url: string | null;
				username: string | null;
				username_display: string | null;
				providers: string[];
			}>(API.auth.me(), { timeoutMs: BOOTSTRAP_TIMEOUT_MS });
			authStore.setUser({
				id: user.id,
				display_name: user.display_name,
				role: user.role as 'admin' | 'trusted' | 'user',
				email: user.email,
				avatar_url: user.avatar_url,
				username: user.username,
				username_display: user.username_display,
				providers: user.providers ?? []
			});
		} catch (cause) {
			if (cause instanceof ApiError && cause.status === 401) {
				if (browser && localStorage.getItem(LAST_USER_ID_KEY)) {
					await resetQueryCacheForUserSwitch();
					clearUserScopedLocalCaches();
					musicSourceStore.reset();
					scrobbleManager.reset();
					localStorage.removeItem(LAST_USER_ID_KEY);
				}
				authStore.clear();
			} else {
				throw error(503, BUSY_MESSAGE);
			}
		}
		authStore.markInitialized();
	}

	// initialized stays true after in-app login; reset persisted caches on account switches
	if (browser && authStore.user) {
		const lastId = localStorage.getItem(LAST_USER_ID_KEY);
		if (lastId && lastId !== authStore.user.id) {
			await resetQueryCacheForUserSwitch();
			clearUserScopedLocalCaches();
			musicSourceStore.reset();
			await scrobbleManager.refreshSettings();
		}
		localStorage.setItem(LAST_USER_ID_KEY, authStore.user.id);
	}

	if (!setupRequired && !isAuthFree && !authStore.isAuthenticated) {
		throw redirect(302, '/login');
	}

	// the primary source is user-specific; connection defaults are global
	let primarySource = DEFAULT_SOURCE;
	if (authStore.isAuthenticated) {
		try {
			const data = await api.global.get<{ primary_music_source: unknown }>(
				API.me.scrobblePreferences(),
				{ timeoutMs: BOOTSTRAP_TIMEOUT_MS }
			);
			if (isMusicSource(data.primary_music_source)) primarySource = data.primary_music_source;
		} catch {
			primarySource = DEFAULT_SOURCE;
		}
	}

	return { primarySource };
};
