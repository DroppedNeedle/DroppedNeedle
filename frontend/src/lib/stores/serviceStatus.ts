import { writable } from 'svelte/store';

interface ServiceDegradation {
	[source: string]: string;
}

const CLEAR_DELAY_MS = 10_000;

function createServiceStatusStore() {
	const { subscribe, set, update } = writable<ServiceDegradation>({});
	let clearTimer: ReturnType<typeof setTimeout> | null = null;

	function scheduleClear() {
		if (clearTimer !== null) clearTimeout(clearTimer);
		clearTimer = setTimeout(() => set({}), CLEAR_DELAY_MS);
	}

	return {
		subscribe,

		/** Merge the degraded sources one answer reported. */
		recordFromResponse(serviceStatus: ServiceDegradation) {
			if (Object.keys(serviceStatus).length === 0) return;
			update((current) => ({ ...current, ...serviceStatus }));
			scheduleClear();
		},

		/** Dismiss all degradation signals (user action or navigation). */
		clear() {
			if (clearTimer !== null) clearTimeout(clearTimer);
			set({});
		}
	};
}

export const serviceStatusStore = createServiceStatusStore();

function serviceStatusOf(data: unknown): ServiceDegradation | null {
	if (!data || typeof data !== 'object' || !('service_status' in data)) return null;
	const status = data.service_status;
	if (!status || typeof status !== 'object' || Array.isArray(status)) return null;
	return status as ServiceDegradation;
}

/**
 * Records the `service_status` a degraded answer carries. The query and
 * mutation caches call it for every success, so no hook reads it by hand; a
 * paged query's answer is checked page by page.
 */
export function recordServiceStatus(data: unknown): void {
	const pages =
		data && typeof data === 'object' && 'pages' in data && Array.isArray(data.pages)
			? data.pages
			: [data];
	for (const page of pages) {
		const status = serviceStatusOf(page);
		if (status) serviceStatusStore.recordFromResponse(status);
	}
}
