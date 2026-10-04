import { goto } from '$app/navigation';
import { resolve } from '$app/paths';
import { api } from '$lib/api/client';
import { AUTH_ENDPOINTS } from '$lib/queries/auth/endpoints';
import { clearUserSessionState } from '$lib/utils/userSessionCleanup';

// Clears browser-wide cache before navigating so the next user on a shared browser
// sees no prior personalized data; local state clears regardless of network success.
export async function logout(): Promise<void> {
	try {
		await api.global.v3.POST(AUTH_ENDPOINTS.logout);
	} catch {
		// A failed revoke must not strand the user in a signed-in UI.
	}
	await clearUserSessionState().catch(() => undefined);
	await goto(resolve('/login'));
}
