// Handing the browser back to a native app (app connect, /connect).

/**
 * `value` when it is an address the browser may hand to an app: an https
 * address or a private-use scheme named after a domain (RFC 8252 7.1), as
 * the server lists them. No script, data or other app-less address comes
 * through here, whatever the server answered.
 */
export function appAddress(value: unknown): string | null {
	if (typeof value !== 'string') return null;
	const scheme = /^([a-z][a-z0-9+.-]*):/i.exec(value)?.[1]?.toLowerCase();
	if (!scheme) return null;
	return scheme === 'https' || scheme.includes('.') ? value : null;
}

/**
 * Leave for `address`. Called within the user's click: a browser hands an
 * app address over only while the click's user activation lasts.
 */
export function handBack(address: string): void {
	window.location.assign(address);
}

/**
 * Whether this page is shown inside another page's frame. The server's
 * `frame-ancestors` policy already refuses that; this is the page's own
 * check, so a confirmation is never offered where it could be clicked
 * through from outside.
 */
export function framed(): boolean {
	try {
		return window.top !== window.self;
	} catch {
		return true;
	}
}
