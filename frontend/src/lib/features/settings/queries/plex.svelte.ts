import type { components } from '$lib/api/v3/openapi';
import { key, userScope } from '$lib/queries/keys';
import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { ApiError } from '$lib/api/errors';
import { authStore } from '$lib/stores/authStore.svelte';
import { readNow } from '$lib/queries/readNow';
import { toastStore } from '$lib/stores/toast';
import { createAppMutation } from '$lib/queries/effects';

// The three legs of the single Plex OAuth flow: `login` signs a session in,
// `link` attaches a media-server account to the current user, `connect`
// authorizes the server itself from admin settings.
export type PlexPurpose = 'login' | 'link' | 'connect';

export type PlexStartBody = components['schemas']['PlexStartBody'];
export type PlexLoginPollBody = components['schemas']['PlexLoginPollBody'];
export type PlexLoginPollResult = components['schemas']['PlexLoginPollResult'];
// Link and connect polls share the one-pin body; only the login leg names
// its own.
export type PlexPinBody = components['schemas']['PlexPinBody'];
export type PlexLinkPollResult = components['schemas']['PlexLinkPollResult'];
export type PlexConnectPollResult = components['schemas']['PlexConnectPollResult'];

export type PlexPollResult<P extends PlexPurpose> = P extends 'login'
	? PlexLoginPollResult
	: P extends 'link'
		? PlexLinkPollResult
		: PlexConnectPollResult;

export const PlexQueryKeyFactory = {
	libraries: (userId: string | null | undefined) => key(userScope(userId), 'plex', 'libraries')
};

// Plain transport for the single Plex OAuth flow. TanStack mutations wrap
// these, and interval pollers call pollPlexFlow directly
// so a poll tick stays a read with no mutation bookkeeping.

// Each start hands back a secret that proves this browser started the PIN;
// every poll must send it. It lives in memory only (never in a URL or in
// storage) and is dropped once the flow completes.
const pinSecrets = new Map<number, string>(); // eslint-disable-line svelte/prefer-svelte-reactivity

export async function startPlexFlow(purpose: PlexPurpose): Promise<PlexStartBody> {
	const started = await api.v3.POST(
		purpose === 'login'
			? v3('/api/v3/auth/plex/start')
			: purpose === 'link'
				? v3('/api/v3/auth/plex/start/link')
				: v3('/api/v3/auth/plex/start/connect')
	);
	pinSecrets.set(started.pin_id, started.pin_secret);
	return started;
}

export async function pollPlexFlow<P extends PlexPurpose>(
	purpose: P,
	pinId: number
): Promise<PlexPollResult<P>> {
	// The poll route is one literal per purpose; the union result narrows to
	// the caller's purpose at runtime, so the cast only recovers the static
	// link the generic erases.
	// A limited poll says nothing about the PIN: it is still pending, as the
	// server treats a poll while plex.tv is paced, and the next tick asks again.
	const result = await api.v3
		.POST(
			purpose === 'login'
				? v3('/api/v3/auth/plex/poll/login')
				: purpose === 'link'
					? v3('/api/v3/auth/plex/poll/link')
					: v3('/api/v3/auth/plex/poll/connect'),
			{
				pin_id: pinId,
				pin_secret: pinSecrets.get(pinId) ?? ''
			}
		)
		.catch((error: unknown) => {
			if (error instanceof ApiError && error.status === 429) return { completed: false };
			throw error;
		});
	if (result.completed) pinSecrets.delete(pinId);
	return result as PlexPollResult<P>;
}

/** The saved Plex server's libraries, read when the settings tab asks. */
export const fetchPlexLibraries = () =>
	readNow(PlexQueryKeyFactory.libraries(authStore.user?.id), (signal) =>
		api.v3.GET(v3('/api/v3/settings/plex/libraries'), { signal })
	);

export interface PlexStartVars {
	purpose: PlexPurpose;
}

function errorMessage(err: unknown, fallback: string): string {
	return err instanceof Error && err.message ? err.message : fallback;
}

// Minting a pin changes no state, so start carries no invalidation.
export const createPlexStartMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (vars: PlexStartVars): Promise<PlexStartBody> => startPlexFlow(vars.purpose),
		onError: (err) =>
			toastStore.show({
				message: errorMessage(err, 'Could not reach Plex. Try again.'),
				type: 'error'
			})
	}));
