import { base } from '$app/paths';

/**
 * Deployment base path (e.g. '' or '/music'), read from SvelteKit's resolved
 * `paths.base`. The Docker build bakes a fixed `/__DROPPEDNEEDLE_BASE__`
 * placeholder here and container startup replaces those literal bytes with the
 * validated BASE_PATH, so this value is authoritative at runtime.
 */

function splitSuffix(path: string): { pathPart: string; suffix: string } {
	const idx = path.search(/[?#]/);
	if (idx === -1) return { pathPart: path, suffix: '' };
	return { pathPart: path.slice(0, idx), suffix: path.slice(idx) };
}

/**
 * The deployment base prefix on its own ('' when undeployed at root):
 * tolerates a trailing slash on the configured base while '' or '/'
 * stays empty. Shared by the path prefix helpers and the v3 client base URL.
 */
export function basePathPrefix(): string {
	return !base || base === '/' ? '' : base.replace(/\/+$/, '');
}

/**
 * Prefixes an internal root-relative path (`/api/v1/...`, media, SSE) with the
 * deployment base exactly once. Idempotent and segment-aware: an already
 * prefixed path is returned unchanged, a duplicated base collapses back to one,
 * and lookalike first segments (`/dnx` when base is `/dn`) still get prefixed.
 *
 * External and pseudo schemes (`https:`, `data:`, `blob:`, `mailto:`, ...),
 * protocol-relative URLs (`//cdn.example.com/x`), hash links, relative paths,
 * and empty strings pass through verbatim. Query strings and fragments are
 * preserved on transformed paths.
 *
 * Empty base is byte-compatible: every input maps to itself unchanged.
 */
export function withBasePath(path: string): string {
	if (!path || !path.startsWith('/') || path.startsWith('//')) {
		return path;
	}

	const basePath = basePathPrefix();
	if (!basePath) {
		return path;
	}

	const { pathPart, suffix } = splitSuffix(path);

	// Collapse repetitions of the exact base tail (double application) while
	// keeping everything else about the path verbatim.
	const baseTail = basePath.slice(1);
	let rest = pathPart.slice(1);
	let stripped = false;
	let keptTrailingSlash = false;
	while (rest.startsWith(baseTail)) {
		const after = rest.slice(baseTail.length);
		if (after === '') {
			stripped = true;
			keptTrailingSlash = pathPart.endsWith('/');
			rest = '';
			break;
		}
		if (!after.startsWith('/')) {
			break; // lookalike segment such as '/dnextra' — not the base
		}
		stripped = true;
		rest = after.slice(1);
	}

	if (!stripped) {
		// Not based yet — the ordinary prefixing case.
		return `${basePath}${pathPart}${suffix}`;
	}

	const remainder = rest === '' ? (keptTrailingSlash ? '/' : '') : `/${rest}`;
	return `${basePath}${remainder}${suffix}`;
}

/**
 * Removes exactly one deployment base prefix — the inverse of
 * {@link withBasePath} — for active-route comparisons: pass
 * `page.url.pathname` through it before comparing against root-relative
 * route paths ('/', '/artists', ...) so navigation state matches whatever
 * base the app is deployed under. Exactly one occurrence is removed, never
 * more ('/dn/dn/x' -> '/dn/x'); paths that do not carry the base come back
 * verbatim, including lookalikes ('/dnextra' under base '/dn'), external
 * URLs, protocol-relative URLs, relative strings, hash links, and ''.
 * Query strings and fragments ride through unchanged.
 *
 * Empty base is byte-compatible: every input maps to itself unchanged.
 */
export function withoutBasePath(pathname: string): string {
	if (!pathname || !pathname.startsWith('/') || pathname.startsWith('//')) {
		return pathname;
	}

	const basePath = basePathPrefix();
	if (!basePath) {
		return pathname;
	}

	const { pathPart, suffix } = splitSuffix(pathname);
	let strippedPart: string | null = null;
	if (pathPart.startsWith(`${basePath}/`)) {
		strippedPart = pathPart.slice(basePath.length); // keeps the leading '/'
	} else if (pathPart === basePath) {
		strippedPart = '/';
	}

	return strippedPart === null ? pathname : `${strippedPart}${suffix}`;
}
