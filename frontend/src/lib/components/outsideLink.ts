// The one definition of an address that may leave this app as a link.
// Anything not built here (provider, plugin and remote-server data) is held
// to it before it reaches an href, a frame or a window.
//
// Absolute web addresses only. The patterns are anchored on the raw value,
// so whitespace or control characters before the scheme fail, as do
// relative and scheme-relative addresses, which would point into this app.
export const WEB_URL = /^https?:\/\//i;

// Prose (notes, release notes) may also link an email address.
export const PROSE_LINK_URL = /^(?:https?:\/\/|mailto:)/i;

// The address when it is safe to bind as a link, otherwise undefined, so
// the caller renders no link at all.
export function outsideLink(address: string | null | undefined): string | undefined {
	return typeof address === 'string' && WEB_URL.test(address) ? address : undefined;
}
