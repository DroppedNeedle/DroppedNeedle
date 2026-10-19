import { ApiError, api } from '$lib/api/client';

/**
 * Download a file through the api client so failures surface as errors
 * (anchor-click downloads fail silently in the browser). The
 * server's Content-Disposition filename is used unless `filename` is given.
 * Throws ApiError on HTTP errors and TransportError on network failure -
 * callers turn these into toasts.
 */
export async function downloadBlob(url: string, filename?: string): Promise<void> {
	const response = await api.global.raw(url);
	if (!response.ok) {
		throw new ApiError(response.status, `Download failed with status ${response.status}`);
	}
	const blob = await response.blob();
	const disposition = response.headers.get('content-disposition') ?? '';
	const resolvedFilename =
		filename ?? disposition.match(/filename="?([^";]+)"?/)?.[1] ?? 'download';
	const objectUrl = URL.createObjectURL(blob);
	try {
		const link = document.createElement('a');
		link.href = objectUrl;
		link.download = resolvedFilename;
		document.body.appendChild(link);
		link.click();
		link.remove();
	} finally {
		URL.revokeObjectURL(objectUrl);
	}
}
