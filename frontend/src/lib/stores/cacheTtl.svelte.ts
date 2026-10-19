import { browser } from '$app/environment';
import { api } from '$lib/api/client';
import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';

/**
 * The server settings the app reads once at start. The browser's cache
 * lifetimes are fixed in `CACHE_TTL` ($lib/constants), not settings.
 */
export interface CacheTTLs {
	discoverQueueAutoGenerate: boolean;
}

const DEFAULTS: CacheTTLs = {
	discoverQueueAutoGenerate: true
};

let resolved = $state<CacheTTLs>({ ...DEFAULTS });
let initialized = false;

export async function initCacheTTLs(): Promise<void> {
	if (!browser || initialized) return;
	initialized = true;

	try {
		const data = await api.global.v3.GET(SETTINGS_ENDPOINTS.cacheTtls());
		resolved = {
			discoverQueueAutoGenerate:
				data.discover_queue_auto_generate ?? DEFAULTS.discoverQueueAutoGenerate
		};
	} catch {
		resolved = { ...DEFAULTS };
	}
}

export function getCacheTTLs(): CacheTTLs {
	return resolved;
}
