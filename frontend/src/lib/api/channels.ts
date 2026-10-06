import { getApiUrl } from './api-utils';

// Server channels other than request and response: event streams and
// unload beacons. They live in the transport so the lint rule can keep
// EventSource and sendBeacon out of the rest of the app.

/** Opens a server-sent event stream for one API path. */
export function openEventStream(path: string): EventSource {
	return new EventSource(getApiUrl(path));
}

/** Whether this browser can send an unload beacon. */
export function beaconAvailable(): boolean {
	return typeof navigator !== 'undefined' && typeof navigator.sendBeacon === 'function';
}

/** Queues a JSON body for delivery while the page unloads. */
export function sendJsonBeacon(url: string, payload: Record<string, unknown>): boolean {
	return navigator.sendBeacon(
		url,
		new Blob([JSON.stringify(payload)], { type: 'application/json' })
	);
}
