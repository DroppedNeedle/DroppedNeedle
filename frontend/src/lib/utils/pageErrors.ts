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
