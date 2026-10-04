import { createMutation } from '@tanstack/svelte-query';

import { toastStore } from '$lib/stores/toast';

import { pollPlexFlow, startPlexFlow } from './PlexFlowApi';
import type { PlexPollResult, PlexPurpose, PlexStartBody } from './types';

export interface PlexStartVars {
	purpose: PlexPurpose;
}

function errorMessage(err: unknown, fallback: string): string {
	return err instanceof Error && err.message ? err.message : fallback;
}

// Minting a pin changes no state, so start carries no invalidation.
export const createPlexStartMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: PlexStartVars): Promise<PlexStartBody> => startPlexFlow(vars.purpose),
		onError: (err) =>
			toastStore.show({
				message: errorMessage(err, 'Could not reach Plex. Try again.'),
				type: 'error'
			})
	}));

// Polling is a read: completion effects (store the session, refresh the
// account list, keep the token) belong to the calling purpose, not the flow.
export const createPlexPollMutation = <P extends PlexPurpose>(purpose: P) =>
	createMutation(() => ({
		mutationFn: (pinId: number): Promise<PlexPollResult<P>> => pollPlexFlow(purpose, pinId)
	}));
