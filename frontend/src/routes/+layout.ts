import { browser } from '$app/environment';
import { api } from '$lib/api/client';
import { API, AUTH_FREE_PATHS } from '$lib/constants';
import { resetQueryCacheForUserSwitch } from '$lib/queries/QueryClient';
import { DEFAULT_SOURCE, isMusicSource, musicSourceStore } from '$lib/stores/musicSource';
import { scrobbleManager } from '$lib/stores/scrobble.svelte';
import { authStore, LAST_USER_ID_KEY } from '$lib/stores/authStore.svelte';
import { clearUserScopedLocalCaches } from '$lib/utils/userScopedCaches';
import { redirect } from '@sveltejs/kit';
import type { LayoutLoad } from './$types';

export const ssr = false;
export const prerender = false;

export const load: LayoutLoad = async ({ url }) => {
	const path = url.pathname;
	const isAuthFree = AUTH_FREE_PATHS.some((p) => path.startsWith(p));

	let setupRequired = false;
	try {
		const status = await api.global.get<{ required: boolean }>('/api/v1/auth/setup/status');
		setupRequired = status.required;
	} catch {
		// backend unreachable, let the page handle it
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
			}>('/api/v1/auth/me');
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
		} catch {
			// no valid session; if a user was previously signed in here their session
			// just ended (expiry/revoke), so clear the persisted cache too (AMU-5)
			if (browser && localStorage.getItem(LAST_USER_ID_KEY)) {
				await resetQueryCacheForUserSwitch();
				clearUserScopedLocalCaches();
				musicSourceStore.reset();
				scrobbleManager.reset();
				localStorage.removeItem(LAST_USER_ID_KEY);
			}
			authStore.clear();
		}
		authStore.markInitialized();
	}

	// AMU-5: reconcile the active user on every load, not just first hydration.
	// `authStore.initialized` stays true after a warm in-app login (no reload), so a
	// cold-load-only check would miss an account switch and leak the previous user's
	// browser-wide cache (home/discover are not yet userId-keyed). Same-user reload
	// keeps its cache (ids match).
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

	// per-user primary source (F6/M2): read the requesting user's prefs, not the
	// global admin default; falls back to DEFAULT_SOURCE on missing prefs / error
	let primarySource = DEFAULT_SOURCE;
	if (authStore.isAuthenticated) {
		try {
			const data = await api.global.get<{ primary_music_source: unknown }>(
				API.me.scrobblePreferences()
			);
			if (isMusicSource(data.primary_music_source)) primarySource = data.primary_music_source;
		} catch {
			/* keep DEFAULT_SOURCE fallback */
		}
	}

	return { primarySource };
};
