import { get, writable } from 'svelte/store';
import { api } from '$lib/api/client';
import { HOME_ENDPOINTS } from '$lib/queries/charts/endpoints';

interface IntegrationStatus {
	download_client: boolean;
	library: boolean;
	jellyfin: boolean;
	navidrome: boolean;
	plex: boolean;
	youtube: boolean;
	youtube_api: boolean;
	localfiles: boolean;
	loaded: boolean;
}

function createIntegrationStore() {
	const { subscribe, set, update } = writable<IntegrationStatus>({
		download_client: false,
		library: false,
		jellyfin: false,
		navidrome: false,
		plex: false,
		youtube: false,
		youtube_api: false,
		localfiles: false,
		loaded: false
	});
	let loadPromise: Promise<void> | null = null;

	return {
		subscribe,
		setStatus: (status: Partial<IntegrationStatus>) => {
			update((current) => ({ ...current, ...status, loaded: true }));
		},
		setDownloadClientConfigured: (configured: boolean) => {
			update((current) => ({ ...current, download_client: configured }));
		},
		reset: () => {
			set({
				download_client: false,
				library: false,
				jellyfin: false,
				navidrome: false,
				plex: false,
				youtube: false,
				youtube_api: false,
				localfiles: false,
				loaded: false
			});
		},
		ensureLoaded: async () => {
			const current = get({ subscribe });
			if (current.loaded) return;
			if (loadPromise) return loadPromise;

			loadPromise = (async () => {
				try {
					// download_client here means "any acquisition source is set up" (slskd OR
					// Usenet) - the backend integration status is the single source of truth, so a
					// Usenet-only install isn't gated on slskd. Don't re-derive it from the slskd
					// health check alone (that's what made disabled-slskd hide the Request buttons).
					const status = await api.global.v3.GET(HOME_ENDPOINTS.integrationStatus());
					update((state) => ({ ...state, ...status, loaded: true }));
				} catch {
					// Leave `loaded` false so a later ensureLoaded() retries. Stamping it
					// true here poisoned the store for the whole session when the load ran
					// before auth completed - every integration stayed "disabled" until a
					// hard refresh (#155).
				}
			})().finally(() => {
				loadPromise = null;
			});

			return loadPromise;
		}
	};
}

export const integrationStore = createIntegrationStore();
