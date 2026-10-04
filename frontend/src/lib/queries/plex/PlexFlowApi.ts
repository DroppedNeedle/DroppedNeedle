import { api } from '$lib/api/client';

import { PLEX_ENDPOINTS } from './endpoints';
import type { PlexPollResult, PlexPurpose, PlexStartBody } from './types';

// Plain transport for the single Plex OAuth flow. TanStack mutations wrap
// these (PlexAuthMutations), and interval pollers call pollPlexFlow directly
// so a poll tick stays a read with no mutation bookkeeping.
export async function startPlexFlow(purpose: PlexPurpose): Promise<PlexStartBody> {
	return api.global.v3.POST(PLEX_ENDPOINTS.start(purpose));
}

export async function pollPlexFlow<P extends PlexPurpose>(
	purpose: P,
	pinId: number
): Promise<PlexPollResult<P>> {
	// The poll route is one literal per purpose; the union result narrows to
	// the caller's purpose at runtime, so the cast only recovers the static
	// link the generic erases.
	const result = await api.global.v3.POST(PLEX_ENDPOINTS.poll(purpose), { pin_id: pinId });
	return result as PlexPollResult<P>;
}
