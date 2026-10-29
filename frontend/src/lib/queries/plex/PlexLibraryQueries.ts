import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { readNow } from '../readNow';
import { PLEX_ENDPOINTS } from './endpoints';
import { PlexQueryKeyFactory } from './PlexQueryKeyFactory';

/** The saved Plex server's libraries, read when the settings tab asks. */
export const fetchPlexLibraries = () =>
	readNow(PlexQueryKeyFactory.libraries(authStore.user?.id), (signal) =>
		api.v3.GET(PLEX_ENDPOINTS.libraries(), { signal })
	);
