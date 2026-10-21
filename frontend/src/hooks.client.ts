import type { HandleClientError } from '@sveltejs/kit';
import {
	isFailedModuleLoad,
	reloadAfterFailedModuleLoad,
	UNREACHABLE_ERROR
} from '$lib/utils/pageErrors';

// A page whose code failed to download (a dropped connection, a network
// change) reloads once to fetch it again, and meanwhile says so instead of
// reporting a bug in the page.
export const handleError: HandleClientError = ({ error, message }) => {
	if (isFailedModuleLoad(error)) {
		reloadAfterFailedModuleLoad();
		return UNREACHABLE_ERROR;
	}
	console.error(error);
	return { message };
};
