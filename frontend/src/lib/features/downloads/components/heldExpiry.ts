/**
 * When the app next retries a held entry by itself: "at 14:05" today, "on 27 Oct at
 * 14:05" later, and "soon" when no time is set or it has passed (the next pass of the
 * held loop, within five minutes).
 */
export function nextTry(at: number | null | undefined, now: Date = new Date()): string {
	if (at == null || at <= now.getTime()) return 'Tries again by itself soon';
	const date = new Date(at);
	const time = date.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
	if (date.toDateString() === now.toDateString()) return `Tries again by itself at ${time}`;
	const day = date.toLocaleDateString(undefined, { day: 'numeric', month: 'short' });
	return `Tries again by itself on ${day} at ${time}`;
}

/** "Kept until 23 Jan": when a held entry's files are removed, in the viewer's locale. */
export function keptUntil(expiresAt: number, now: Date = new Date()): string {
	const date = new Date(expiresAt);
	return `Kept until ${date.toLocaleDateString(undefined, {
		day: 'numeric',
		month: 'short',
		...(date.getFullYear() === now.getFullYear() ? {} : { year: 'numeric' })
	})}`;
}
