import { api } from '$lib/api/client';
import { ApiError } from '$lib/api/errors';

/**
 * Save a blob the page already holds as a file named `filename`. The object
 * URL is revoked a tick after the click, once the browser has taken the
 * download.
 */
export function saveBlob(blob: Blob, filename: string): void {
	const objectUrl = URL.createObjectURL(blob);
	const link = document.createElement('a');
	link.href = objectUrl;
	link.download = filename;
	document.body.appendChild(link);
	try {
		link.click();
	} finally {
		link.remove();
		setTimeout(() => URL.revokeObjectURL(objectUrl), 0);
	}
}

/**
 * Download a file through the api client so failures surface as errors
 * (anchor-click downloads fail silently in the browser). The
 * server's Content-Disposition filename is used unless `filename` is given.
 * Returns the response, so a caller can read a header after the save.
 * Throws ApiError on HTTP errors and TransportError on network failure -
 * callers turn these into toasts.
 */
export async function downloadBlob(url: string, filename?: string): Promise<Response> {
	const response = await api.raw(url);
	if (!response.ok) {
		throw new ApiError(response.status, `Download failed with status ${response.status}`);
	}
	const blob = await response.blob();
	const disposition = response.headers.get('content-disposition') ?? '';
	const resolvedFilename =
		filename ?? disposition.match(/filename="?([^";]+)"?/)?.[1] ?? 'download';
	saveBlob(blob, resolvedFilename);
	return response;
}
