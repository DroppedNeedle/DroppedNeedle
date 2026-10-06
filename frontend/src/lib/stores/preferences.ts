import { writable } from 'svelte/store';
import type { UserPreferences } from '$lib/types';
import { api } from '$lib/api/client';
import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';

const defaultPreferences: UserPreferences = {
	primary_types: ['album', 'ep', 'single'],
	secondary_types: ['studio']
};

const { subscribe, set, update } = writable<UserPreferences>(defaultPreferences);

async function loadPreferences(): Promise<void> {
	try {
		const prefs = await api.global.v3.GET(SETTINGS_ENDPOINTS.preferences());
		set({ ...defaultPreferences, ...prefs });
	} catch {
		// use defaults on fetch failure
	}
}

async function savePreferences(prefs: UserPreferences): Promise<boolean> {
	try {
		const updated = await api.global.v3.PUT(SETTINGS_ENDPOINTS.preferences(), prefs);
		set({ ...defaultPreferences, ...updated });
		return true;
	} catch {
		return false;
	}
}

export const preferencesStore = {
	subscribe,
	load: loadPreferences,
	save: savePreferences,
	update
};
