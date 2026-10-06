import { createQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { setMusicBrainzSourceScope } from './sourceScope.svelte';

import { MusicBrainzQueryKeyFactory } from './MusicBrainzQueryKeyFactory';
import { MUSICBRAINZ_ENDPOINTS } from './endpoints';
import { toMusicBrainzSettings } from './MusicBrainzAdapters';

export const getMusicBrainzSettingsQuery = () =>
	createQuery(() => {
		const queryKey = MusicBrainzQueryKeyFactory.settings();
		const userId = queryKey[2].user_id;
		return {
			queryKey,
			queryFn: async ({ signal }) => {
				const data = toMusicBrainzSettings(
					await api.global.v3.GET(MUSICBRAINZ_ENDPOINTS.settings(), { signal })
				);
				if ((authStore.user?.id ?? null) === userId) {
					setMusicBrainzSourceScope(data, userId);
				}
				return data;
			}
		};
	});
