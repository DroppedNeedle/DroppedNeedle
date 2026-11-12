// When the login page last started single sign-on by itself, kept in this
// tab's sessionStorage. A sign-in that comes straight back to the login page
// (a session cookie that did not stick) then shows the sign-in methods
// instead of sending the browser round the identity provider forever.
export const SSO_AUTO_START_KEY = 'droppedneedle.ssoAutoStart';
// Long enough to cover one round trip through the provider, short enough
// that a session ending later still starts single sign-on by itself.
const RECENT_MS = 60 * 1000;

/** Note that the login page is starting single sign-on by itself now. */
export function markSsoAutoStart(now = Date.now()): void {
	try {
		sessionStorage.setItem(SSO_AUTO_START_KEY, String(now));
	} catch {
		// storage blocked: the page's own once-per-load guard still holds
	}
}

/** True when the login page started single sign-on by itself under a minute ago. */
export function ssoAutoStartedRecently(now = Date.now()): boolean {
	try {
		const at = Number(sessionStorage.getItem(SSO_AUTO_START_KEY));
		return Number.isFinite(at) && at > 0 && now - at >= 0 && now - at < RECENT_MS;
	} catch {
		return false;
	}
}
