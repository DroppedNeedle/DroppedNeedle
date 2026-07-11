import { browser } from '$app/environment';
import { goto } from '$app/navigation';
import { resolve } from '$app/paths';
import { getApiUrl } from '$lib/api/api-utils';
import { resetQueryCacheForUserSwitch } from '$lib/queries/QueryClient';
import { authStore, LAST_USER_ID_KEY } from '$lib/stores/authStore.svelte';
import { musicSourceStore } from '$lib/stores/musicSource';
import { scrobbleManager } from '$lib/stores/scrobble.svelte';
import { clearUserScopedLocalCaches } from '$lib/utils/userScopedCaches';

// Clears browser-wide cache before navigating so the next user on a shared browser
// sees no prior personalized data; local state clears regardless of network success.
export async function logout(): Promise<void> {
	try {
		await fetch(getApiUrl('/api/v1/auth/logout'), { method: 'POST', credentials: 'include' });
	} catch {
		// A failed revoke must not strand the user in a signed-in UI.
	}
	await resetQueryCacheForUserSwitch();
	// Per-user, non-TanStack state the cache reset doesn't touch.
	clearUserScopedLocalCaches();
	musicSourceStore.reset();
	scrobbleManager.reset();
	if (browser) localStorage.removeItem(LAST_USER_ID_KEY);
	authStore.clear();
	await goto(resolve('/login'));
}
