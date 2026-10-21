import type {
	BinRemoval,
	Emptied,
	RecycleBinSummary
} from '$lib/queries/library/RecycleBinQueries.svelte';
import { formatBytes } from '$lib/utils/formatting';

const KIND_LABEL: Record<BinRemoval['kind'], string> = {
	album: 'Album',
	track: 'Track',
	extras: 'Files not on the edition',
	replaced: 'Replaced by a new copy',
	removed: 'Removed'
};

const DAY_MS = 86_400_000;

function count(n: number, one: string, many = `${one}s`): string {
	return `${n} ${n === 1 ? one : many}`;
}

function daysLeft(expiresAt: number, now: number): string {
	const days = Math.ceil((expiresAt - now) / DAY_MS);
	if (days <= 0) return 'goes today';
	return `${count(days, 'day')} left`;
}

/** "Album · 12 files · 412 MB · 27 days left". */
export function binLine(removal: BinRemoval, now: number): string {
	const parts = [KIND_LABEL[removal.kind], count(removal.files, 'file')];
	if (removal.bytes > 0) parts.push(formatBytes(removal.bytes));
	if (removal.expires_at !== null && removal.expires_at !== undefined && removal.held === 0) {
		parts.push(daysLeft(removal.expires_at, now));
	}
	return parts.join(' · ');
}

/** "Removed 3 Oct 2026". */
export function removedLine(removal: BinRemoval): string {
	return `Removed ${new Date(removal.removed_at).toLocaleDateString(undefined, { dateStyle: 'medium' })}`;
}

/** Notes under a row: files not found, can't be checked, or held. */
export function binNotes(removal: BinRemoval): string[] {
	const notes: string[] = [];
	if (removal.not_found > 0) notes.push(`${count(removal.not_found, 'file')} not found`);
	if (removal.unchecked > 0) notes.push(`${count(removal.unchecked, 'file')} can't be checked`);
	return notes;
}

export function retentionLine(days: number | null | undefined): string {
	if (days === null || days === undefined) return 'Removed files wait here before they go.';
	return `Removed files stay here for ${count(days, 'day')}.`;
}

/** The confirm's sentence, or null when there is nothing to delete. */
export function emptyQuestion(summary: RecycleBinSummary): string | null {
	const parts: string[] = [];
	if (summary.files > 0) {
		parts.push(`${count(summary.files, 'file')} (${formatBytes(summary.bytes)})`);
	}
	if (summary.older_folders > 0) parts.push(count(summary.older_folders, 'older folder'));
	if (parts.length === 0) return null;
	return `Delete ${parts.join(' and ')} for good?`;
}

export function stayingLine(summary: RecycleBinSummary): string | null {
	if (summary.staying === 0) return null;
	const verb = summary.staying === 1 ? 'stays' : 'stay';
	return `${count(summary.staying, 'file')} ${verb}: held, in use or not found.`;
}

const KEPT_WHY: Record<Emptied['kept_files'][number]['why'], string> = {
	changed: 'changed in the bin',
	not_in_bin: 'not in the bin',
	not_plain_file: 'not a file',
	unreadable: "can't be read"
};

/** The toast after a run: what went and what stayed. */
export function emptiedToast(emptied: Emptied): string {
	const parts = [
		`Deleted ${count(emptied.deleted, 'file')} (${formatBytes(emptied.freed_bytes)}).`
	];
	if (emptied.older_folders > 0) {
		parts.push(`Cleared ${count(emptied.older_folders, 'older folder')}.`);
	}
	if (emptied.kept > 0) parts.push(`Kept ${count(emptied.kept, 'file')}.`);
	if (emptied.waited > 0) parts.push(`${count(emptied.waited, 'file')} waiting.`);
	return parts.join(' ');
}

/** "Emptied 21 Oct by Harvey. Kept: 03 Song.flac (changed in the bin)." */
export function lastEmptiedLine(emptied: Emptied): string {
	const when = new Date(emptied.at).toLocaleDateString(undefined, { dateStyle: 'medium' });
	const by = emptied.by ? ` by ${emptied.by}` : '';
	const kept = emptied.kept_files.map((file) => `${file.name} (${KEPT_WHY[file.why]})`);
	return kept.length > 0
		? `Emptied ${when}${by}. Kept: ${kept.join(', ')}.`
		: `Emptied ${when}${by}.`;
}
