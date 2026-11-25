import * as publicEnv from '$env/static/public';
import { withBasePath } from '$lib/utils/basePath';

// PUBLIC_API_URL is set only for `vite dev` against a server on another port
// (env.development.example). It is fixed when the app is built, so the page
// fetches no runtime env file, and a build made without it has none.
const configuredEnv: Record<string, string | undefined> = publicEnv;

/**
 * Normalizes an API path by prepending the deployment base path (once) and the
 * PUBLIC_API_URL origin if it's set. Useful for <img> src tags, Background
 * image URLs, EventSource streams, and anywhere else that builds a URL string
 * instead of going through the API client (which calls this internally, so
 * every standard `api.*`/`api.*` call inherits the same behavior).
 *
 * Root-relative paths (`/api/v3/...`) get the base prefix exactly once;
 * external, data:, blob:, hash, and protocol-relative (`//host/...`) URLs
 * pass through untouched. See
 * {@link withBasePath} for the full prefixing contract.
 *
 * @param path The API path (e.g., '/api/v3/covers/...')
 * @returns The fully qualified API URL or the original path if PUBLIC_API_URL is unset.
 */
export function getApiUrl(path: string): string {
	if (!path.startsWith('/') || path.startsWith('//')) {
		return path;
	}

	const basedPath = withBasePath(path);
	const apiOrigin = configuredEnv.PUBLIC_API_URL;
	if (apiOrigin) {
		return `${apiOrigin.replace(/\/+$/, '')}${basedPath}`;
	}

	return basedPath;
}
