import { env } from '$env/dynamic/public';
import { base } from '$app/paths';

/**
 * Normalizes an API path by prepending the reverse-proxy base path (and the
 * PUBLIC_API_URL dev override, if set). Useful for <img> src tags, EventSource
 * URLs, and other places where the API client isn't automatically resolving the
 * absolute URL. `base` is empty by default, so root-hosted deploys are unaffected.
 *
 * @param path The API path (e.g., '/api/v1/covers/...')
 * @returns The fully qualified API URL, or a base-prefixed path.
 */
export function getApiUrl(path: string): string {
	if (!path.startsWith('/')) {
		return path;
	}

	if (env.PUBLIC_API_URL) {
		const baseUrl = env.PUBLIC_API_URL.replace(/\/$/, '');
		return `${baseUrl}${base}${path}`;
	}

	return `${base}${path}`;
}
