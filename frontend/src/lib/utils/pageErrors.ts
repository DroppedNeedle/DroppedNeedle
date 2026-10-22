/** What the error page says when the server answered that it is busy. */
export const SERVER_BUSY_ERROR: App.Error = {
	kind: 'busy',
	message: 'The server is busy. Your session is safe - try again shortly.'
};

/** What the error page says when a request or part of the app never arrived. */
export const UNREACHABLE_ERROR: App.Error = {
	kind: 'unreachable',
	message: "DroppedNeedle didn't answer. Your session is safe - try again shortly."
};

// Each browser words a failed module download its own way.
const FAILED_MODULE_LOAD = /dynamically imported module|Importing a module script failed/;

/** True when the browser could not download part of the app. */
export function isFailedModuleLoad(error: unknown): boolean {
	return error instanceof TypeError && FAILED_MODULE_LOAD.test(error.message);
}

// static/boot-retry.js covers the page's first download with the same key and
// timings; change them together.
const MODULE_RELOAD_KEY = 'dn-module-reload-at';
// A second failure this soon after a reload shows the error page and stays.
const MODULE_RELOAD_WINDOW_MS = 60_000;
// A network change can come in a burst; let it settle before reloading.
const MODULE_RELOAD_DELAY_MS = 1_000;

/**
 * Reloads the page once after part of the app failed to download. The browser
 * keeps a failed module download for the life of the page, so only a reload
 * fetches the code again. Does nothing when the page already reloaded for this
 * within the last minute, or session storage is unavailable, so an outage that
 * lasts cannot loop.
 */
export function reloadAfterFailedModuleLoad(): void {
	try {
		const now = Date.now();
		const last = Number(sessionStorage.getItem(MODULE_RELOAD_KEY) ?? 0);
		if (now - last < MODULE_RELOAD_WINDOW_MS) return;
		sessionStorage.setItem(MODULE_RELOAD_KEY, String(now));
	} catch {
		return;
	}
	setTimeout(() => location.reload(), MODULE_RELOAD_DELAY_MS);
}
