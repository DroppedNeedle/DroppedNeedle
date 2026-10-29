import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { readNow } from '../readNow';
import { userIdSegment } from '../userKeySegment';
import { PLEX_ENDPOINTS } from './endpoints';

/** The saved Plex server's libraries, read when the settings tab asks. */
export const fetchPlexLibraries = () =>
	readNow(['plex', 'libraries', userIdSegment(authStore.user?.id)], (signal) =>
		api.v3.GET(PLEX_ENDPOINTS.libraries(), { signal })
	);
