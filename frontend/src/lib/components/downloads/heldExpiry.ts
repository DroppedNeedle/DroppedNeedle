/** "Kept until 23 Jan": when a held entry's files are removed, in the viewer's locale. */
export function keptUntil(expiresAt: number, now: Date = new Date()): string {
	const date = new Date(expiresAt);
	return `Kept until ${date.toLocaleDateString(undefined, {
		day: 'numeric',
		month: 'short',
		...(date.getFullYear() === now.getFullYear() ? {} : { year: 'numeric' })
	})}`;
}
