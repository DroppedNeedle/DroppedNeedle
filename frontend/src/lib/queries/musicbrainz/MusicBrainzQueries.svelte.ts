import { createQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { authStore } from '$lib/stores/authStore.svelte';
import { setMusicBrainzSourceScope } from './sourceScope.svelte';

import { MusicBrainzQueryKeyFactory } from './MusicBrainzQueryKeyFactory';
import type { MusicBrainzSettingsResponse } from './types';

export const getMusicBrainzSettingsQuery = () =>
	createQuery(() => {
		const queryKey = MusicBrainzQueryKeyFactory.settings();
		const userId = queryKey[2].user_id;
		return {
			queryKey,
			queryFn: async ({ signal }) => {
				const data = await api.global.get<MusicBrainzSettingsResponse>(API.settingsMusicbrainz(), {
					signal
				});
				if ((authStore.user?.id ?? null) === userId) {
					setMusicBrainzSourceScope(data, userId);
				}
				return data;
			}
		};
	});
