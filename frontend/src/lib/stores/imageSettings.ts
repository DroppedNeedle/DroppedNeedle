import { writable } from 'svelte/store';
import { api } from '$lib/api/client';
import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';

interface ImageSettings {
	directRemoteImagesEnabled: boolean;
}

const defaultSettings: ImageSettings = {
	directRemoteImagesEnabled: true
};

const { subscribe, set } = writable<ImageSettings>(defaultSettings);

let lastFetch = 0;
const CACHE_MS = 60_000;

async function load(): Promise<void> {
	const now = Date.now();
	if (now - lastFetch < CACHE_MS) return;

	try {
		const data = await api.global.v3.GET(SETTINGS_ENDPOINTS.advanced());
		set({
			directRemoteImagesEnabled: data.direct_remote_images_enabled ?? true
		});
		lastFetch = now;
	} catch {
		// use defaults on fetch failure
	}
}

export const imageSettingsStore = {
	subscribe,
	load
};
