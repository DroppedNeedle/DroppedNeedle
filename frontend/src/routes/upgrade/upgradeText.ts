import type { components } from '$lib/api/v3/openapi';

type Feature = components['schemas']['UpgradeCallbackFeature'];
type Address = components['schemas']['UpgradeAddress'];
type Code = components['schemas']['UpgradeCode'];
type Step = components['schemas']['UpgradeStep'];
type Task = components['schemas']['UpgradeTask'];

export const FEATURE_LABELS: Record<Feature, string> = {
	single_sign_on: 'Single sign-on',
	spotify: 'Spotify playlist import',
	musicbrainz_contributions: 'MusicBrainz edits'
};

/** The kinds of record worth naming on their own, in the order shown. */
const HEADLINE_COUNTS: [name: string, label: string][] = [
	['user', 'Users'],
	['playlist', 'Playlists'],
	['request', 'Requests'],
	['play_history', 'Plays'],
	['favorite', 'Favorites'],
	['follow', 'Follows'],
	['library_artist', 'Artists'],
	['library_album', 'Albums'],
	['library_track', 'Songs']
];

/** The headline counts present in `counts`, labelled. */
export function headlineCounts(counts: Record<string, number>): [label: string, count: number][] {
	return HEADLINE_COUNTS.filter(([name]) => (counts[name] ?? 0) > 0).map(([name, label]) => [
		label,
		counts[name] ?? 0
	]);
}

/** Every count, by its report name made readable. */
export function allCounts(counts: Record<string, number>): [label: string, count: number][] {
	return Object.entries(counts)
		.filter(([, count]) => count > 0)
		.map(([name, count]) => [name.replaceAll('_', ' '), count]);
}

/** A path is put after the address this page was opened at. */
export function showAddress(address: Address | null | undefined, origin: string): string {
	if (!address) return '';
	return address.kind === 'full' ? address.value : `${origin}${address.value}`;
}

export const STEP_LABELS: Record<Step, string> = {
	recover: 'Putting back the files of the unfinished upgrade',
	move_aside: 'Moving the v2 files into v2-backup',
	export: 'Exporting from v2',
	export_again: 'Exporting again without records of deleted users',
	dry_run: 'Checking the import',
	import: 'Importing into v3',
	remove_backup: 'Deleting the v2 files',
	roll_back: 'Putting the v2 files back',
	undo: 'Deleting the v3 data and putting the v2 files back'
};

/** The steps a run goes through, in order, for the list on the progress screen. */
export function plannedSteps(
	task: Task,
	step: Step | null | undefined,
	seen: readonly Step[]
): Step[] {
	if (task === 'undo') return ['undo'];
	const start: Step[] = step === 'recover' || seen.includes('recover') ? ['recover'] : [];
	if (task === 'fresh') return [...start, 'move_aside'];
	const again: Step[] =
		step === 'export_again' || seen.includes('export_again') ? ['export_again'] : [];
	return [...start, 'move_aside', 'export', ...again, 'dry_run', 'import'];
}

/** What to do after a run failed, where the tool's sentence does not say. */
const FAILURE_FIXES: Partial<Record<Code, string>> = {
	LOCKED: 'Stop the other server or tool using the data folders, then try again.',
	TOOL_RUNNING: 'Wait for the other upgrade to end, then try again.',
	FILE_ERROR: 'Check that the user in PUID and PGID owns the config and cache folders.',
	OCCUPIED: 'Move away what is in the way, then try again.',
	NOT_ENOUGH_DISK: 'Free some space in the cache folder, then try again.',
	ROLLBACK_FAILED: 'Go back to v2 to put the rest back.',
	CALLBACKS_TO_CHANGE: 'Try again to see the addresses to change.'
};

export function failureFix(code: Code): string {
	return FAILURE_FIXES[code] ?? 'The upgrade guide lists what to do for each message.';
}
