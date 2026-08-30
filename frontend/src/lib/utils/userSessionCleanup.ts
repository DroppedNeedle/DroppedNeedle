import { browser } from '$app/environment';
import { clearPersistedQueryCache } from '$lib/queries/IndexedDbPersister.svelte';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore, LAST_USER_ID_KEY } from '$lib/stores/authStore.svelte';
import { clearUserScopedLocalCaches } from '$lib/utils/userScopedCaches';

type SessionCleanupOptions = {
	clearAuth?: boolean;
	clearLastUserId?: boolean;
};

type SessionReset = () => void;
const registeredSessionResets = new Set<SessionReset>();

/**
 * Register store-local reset legs without making this helper import the stores that depend on
 * the API client. The API 401 path can therefore use the same cleanup coordinator safely.
 */
export function registerUserSessionReset(reset: SessionReset): () => void {
	registeredSessionResets.add(reset);
	return () => registeredSessionResets.delete(reset);
}

function runCleanupLeg(failures: unknown[], cleanup: () => void): void {
	try {
		cleanup();
	} catch (error) {
		failures.push(error);
	}
}

/**
 * Clear every browser session leg before navigation. Synchronous state is cleared before the
 * IndexedDB await, and each leg is isolated so one failure cannot strand the rest.
 */
export async function clearUserSessionState(options: SessionCleanupOptions = {}): Promise<void> {
	const failures: unknown[] = [];
	const { clearAuth = true, clearLastUserId = true } = options;

	runCleanupLeg(failures, clearUserScopedLocalCaches);
	for (const reset of [...registeredSessionResets]) runCleanupLeg(failures, reset);
	if (browser && clearLastUserId) {
		runCleanupLeg(failures, () => localStorage.removeItem(LAST_USER_ID_KEY));
	}
	runCleanupLeg(failures, () => queryClient.clear());
	if (clearAuth) runCleanupLeg(failures, () => authStore.clear());

	try {
		await clearPersistedQueryCache();
	} catch (error) {
		failures.push(error);
	}

	if (failures.length === 1) throw failures[0];
	if (failures.length > 1) throw new AggregateError(failures, 'Session cleanup failed');
}
