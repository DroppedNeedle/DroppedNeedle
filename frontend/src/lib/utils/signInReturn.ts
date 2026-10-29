import { AUTH_FREE_PATHS } from '$lib/constants';

// Where the browser goes back to after signing in, for a page that sent it
// to the sign-in page (a deep link, or an app asking at /connect). Kept in
// sessionStorage: one tab's own, and it survives the round trip through an
// identity provider in that tab.
const STORAGE_KEY = 'droppedneedle.signInReturn';
// An address older than this is not followed: the user may have walked
// away from that sign-in and started another.
const MAX_AGE_MS = 15 * 60 * 1000;

/**
 * `path` when it is a page inside this app (no base path) that sign-in may
 * return to: root-relative, never protocol-relative, no backslash or
 * control character a browser could read as another host, and not a
 * sign-in page itself. Otherwise null.
 */
export function returnPath(path: string): string | null {
	if (!path.startsWith('/') || path.startsWith('//')) return null;
	// eslint-disable-next-line no-control-regex
	if (/[\\\u0000-\u001f\u007f]/.test(path)) return null;
	const pathname = path.split(/[?#]/, 1)[0] ?? '';
	if (pathname === '/' || AUTH_FREE_PATHS.some((free) => pathname.startsWith(free))) return null;
	return path;
}

/** Remember `path` (with its query) to return to after signing in. */
export function rememberSignInReturn(path: string, now = Date.now()): void {
	const kept = returnPath(path);
	try {
		if (kept) sessionStorage.setItem(STORAGE_KEY, JSON.stringify({ path: kept, at: now }));
		else sessionStorage.removeItem(STORAGE_KEY);
	} catch {
		// storage blocked: sign-in lands on the home page as before
	}
}

/** The page to land on after signing in, once: the remembered one, else `/`. */
export function takeSignInReturn(now = Date.now()): string {
	try {
		const raw = sessionStorage.getItem(STORAGE_KEY);
		sessionStorage.removeItem(STORAGE_KEY);
		if (!raw) return '/';
		const saved: unknown = JSON.parse(raw);
		if (typeof saved !== 'object' || saved === null) return '/';
		const { path, at } = saved as { path?: unknown; at?: unknown };
		if (typeof path !== 'string' || typeof at !== 'number') return '/';
		if (now - at > MAX_AGE_MS || now < at) return '/';
		return returnPath(path) ?? '/';
	} catch {
		return '/';
	}
}
