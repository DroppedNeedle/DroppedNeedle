import { api } from '$lib/api/client';
import { ApiError } from '$lib/api/errors';

import { PLEX_ENDPOINTS } from './endpoints';
import type { PlexPollResult, PlexPurpose, PlexStartBody } from './types';

// Plain transport for the single Plex OAuth flow. TanStack mutations wrap
// these (PlexAuthMutations), and interval pollers call pollPlexFlow directly
// so a poll tick stays a read with no mutation bookkeeping.

// Each start hands back a secret that proves this browser started the PIN;
// every poll must send it. It lives in memory only (never in a URL or in
// storage) and is dropped once the flow completes.
const pinSecrets = new Map<number, string>();

export async function startPlexFlow(purpose: PlexPurpose): Promise<PlexStartBody> {
	const started = await api.v3.POST(PLEX_ENDPOINTS.start(purpose));
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
		.POST(PLEX_ENDPOINTS.poll(purpose), {
			pin_id: pinId,
			pin_secret: pinSecrets.get(pinId) ?? ''
		})
		.catch((error: unknown) => {
			if (error instanceof ApiError && error.status === 429) return { completed: false };
			throw error;
		});
	if (result.completed) pinSecrets.delete(pinId);
	return result as PlexPollResult<P>;
}
