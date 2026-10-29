import { toastStore } from '$lib/stores/toast';

import { startPlexFlow } from './PlexFlowApi';
import type { PlexPurpose, PlexStartBody } from './types';
import { createAppMutation } from '$lib/queries/effects';

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
