import type { AlbumRemoval } from '$lib/queries/library/RemovalQueries.svelte';

const KIND_LABEL: Record<AlbumRemoval['kind'], string> = {
	album: 'Album',
	track: 'Track',
	extras: 'Files not on the edition'
};

function files(n: number): string {
	return n === 1 ? '1 file' : `${n} files`;
}

/** "Album · 12 files · removed 3 Oct 2026". */
export function removalLine(removal: AlbumRemoval): string {
	const when = new Date(removal.removed_at).toLocaleDateString(undefined, { dateStyle: 'medium' });
	return `${KIND_LABEL[removal.kind]} · ${files(removal.files)} · removed ${when}`;
}

export function putBackToast(n: number): string {
	return `Put back ${files(Math.max(n, 0))}.`;
}

/** The files a put-back left out, by name, or null when none. */
export function notBackLine(missing: string[]): string | null {
	if (missing.length === 0) return null;
	const names = missing.map((path) => path.split('/').pop() || path);
	return `Not in the recycle bin any more: ${names.join(', ')}`;
}
