// Every sentence the edition plate and the edition sheet show, with their
// plural forms. Numbers are only ever counts of the person's own files and
// tracks.
import { ApiError } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { formatDuration } from '$lib/utils/formatting';

type EditionStatus = components['schemas']['EditionStatusView'];
type EditionPlan = components['schemas']['EditionPlanView'];
type TagChange = components['schemas']['EditionTagChangeView'];
type TagWriteFile = components['schemas']['EditionTagFileView'];
type TagWriteStep = components['schemas']['EditionTagsStep'];
type Reason = components['schemas']['ReasonView'];

/** The facts an edition is named and described by, from any source. */
export interface EditionFactsLike {
	title?: string | null;
	disambiguation?: string | null;
	date?: string | null;
	country?: string | null;
	formats?: string[] | null;
	track_count?: number | null;
}

export function count(n: number, one: string, many: string): string {
	return `${n} ${n === 1 ? one : many}`;
}

/** `CD`, `2×CD`, `CD + DVD`: one format per medium, repeats folded. */
export function formatsLabel(formats: string[] | null | undefined): string {
	const runs: { format: string; times: number }[] = [];
	for (const format of formats ?? []) {
		const last = runs.at(-1);
		if (last && last.format === format) last.times += 1;
		else runs.push({ format, times: 1 });
	}
	return runs.map((run) => (run.times > 1 ? `${run.times}×${run.format}` : run.format)).join(' + ');
}

export function editionYear(date: string | null | undefined): string | null {
	return date?.slice(0, 4) || null;
}

/** Line 1 everywhere: the disambiguation, else "{first format} edition". */
export function editionName(facts: EditionFactsLike): string {
	const disambiguation = facts.disambiguation?.trim();
	if (disambiguation) return disambiguation.charAt(0).toUpperCase() + disambiguation.slice(1);
	const format = facts.formats?.[0];
	if (format) return `${format} edition`;
	return facts.title?.trim() || 'Untitled edition';
}

/** A list row's line 2: "{year} · {country} · {formats} · {n} tracks". */
export function editionFactsLine(facts: EditionFactsLike): string {
	return [
		editionYear(facts.date),
		facts.country,
		formatsLabel(facts.formats),
		facts.track_count ? count(facts.track_count, 'track', 'tracks') : null
	]
		.filter(Boolean)
		.join(' · ');
}

/** A list row's line 3. Null when nothing is known about the fit. */
export function fitLine(kept: number, files: number): string | null {
	if (files <= 0) return null;
	if (kept <= 0) return 'Fits none of your files';
	if (kept >= files) return files === 1 ? 'Fits your 1 file' : `Fits all ${files} of your files`;
	return `Fits ${kept} of your ${files} files`;
}

export function planFit(plan: EditionPlan): string | null {
	return fitLine(plan.kept_count, plan.files.length);
}

/** Tracks of the edition no file holds in any version. */
export function trulyMissing(plan: EditionPlan): number {
	return Math.max(0, plan.missing_count - plan.other_version_count);
}

const isAre = (n: number) => (n === 1 ? 'is' : 'are');

/** The detail pane's summary sentence. */
export function summarySentence(plan: EditionPlan): string {
	const files = plan.files.length;
	const kept = plan.kept_count;
	if (files > 0 && kept === 0) {
		return 'None of your files fit this edition, so it is probably a different album.';
	}
	const parts: string[] = [];
	if (kept === files) {
		parts.push(
			files === 1
				? 'Your 1 file fits this edition.'
				: `All ${files} of your files fit this edition.`
		);
	} else {
		parts.push(`${kept} of your ${files} files fit this edition.`);
	}
	const missing = trulyMissing(plan);
	if (missing) parts.push(`${missing} of its tracks ${isAre(missing)} missing.`);
	const versions = plan.other_version_count;
	if (versions) {
		parts.push(`${versions} of your files ${isAre(versions)} another version of a track on it.`);
	}
	if (plan.extra_count) {
		parts.push(
			`${plan.extra_count} of your files ${plan.extra_count === 1 ? "isn't" : "aren't"} on it.`
		);
	}
	if (plan.duplicate_count) {
		parts.push(
			plan.duplicate_count === 1
				? '1 is a second copy.'
				: `${plan.duplicate_count} are second copies.`
		);
	}
	return parts.join(' ');
}

/** True when choosing the edition would place none of the files. */
export function fitsNoFile(plan: EditionPlan): boolean {
	return plan.files.length > 0 && plan.kept_count === 0;
}

export type DiscTone = 'accent' | 'warning' | 'none' | 'busy';

export interface PlateCopy {
	line1: string;
	chip: { label: string; kind: 'yours' | 'picked' | 'guess' } | null;
	line2: string;
	/** Appended to line 2 in the warning colour. */
	warnings: string[];
	action: 'Change' | 'Choose';
	disc: { tone: DiscTone; fill: number; spinning: boolean };
	label: string;
}

function chipFor(status: EditionStatus): PlateCopy['chip'] {
	if (status.state === 'chosen') return { label: 'Your choice', kind: 'yours' };
	if (status.state === 'confirmed') return { label: 'Picked by DroppedNeedle', kind: 'picked' };
	if (status.state === 'unconfirmed') return { label: 'Best guess', kind: 'guess' };
	return null;
}

/** Line 2 while the edition's files are placed or written, if they are. */
export function busyLine(status: EditionStatus): string | null {
	if (status.placing) {
		return status.placing.attempts > 0 ? 'Waiting for MusicBrainz' : 'Matching your files to it...';
	}
	if (status.writing) return 'Writing tags and file names...';
	return null;
}

/**
 * The plate's copy. `plan` is the album's own edition plan when it has
 * loaded; `fileCount` is the number of files the album holds.
 */
export function plateCopy(
	status: EditionStatus,
	plan: EditionPlan | null,
	fileCount: number
): PlateCopy {
	const busy = busyLine(status);
	let copy: Omit<PlateCopy, 'label'>;
	if (status.edition_kind === 'custom') {
		copy = {
			line1: 'Your own track list',
			chip: chipFor(status),
			line2: busy ?? `Kept as your files are · ${count(fileCount, 'track', 'tracks')}`,
			warnings: [],
			action: 'Change',
			disc: { tone: busy ? 'busy' : 'accent', fill: 1, spinning: Boolean(busy) }
		};
	} else if (status.edition_kind === 'release') {
		const total = plan?.tracks.length ?? 0;
		const kept = plan?.kept_count ?? 0;
		const missing = plan ? trulyMissing(plan) : 0;
		const versions = plan?.other_version_count ?? 0;
		const facts = plan
			? [
					editionYear(plan.edition.date) ? `Released ${editionYear(plan.edition.date)}` : null,
					plan.edition.country,
					formatsLabel(plan.edition.formats),
					`${kept} of ${count(total, 'track', 'tracks')}`
				]
					.filter(Boolean)
					.join(' · ')
			: '';
		const warnings = busy
			? []
			: [
					missing ? `${missing} missing` : null,
					versions ? `${versions} in another version` : null
				].filter((text): text is string => Boolean(text));
		const complete = Boolean(plan) && kept >= total && missing === 0 && versions === 0;
		copy = {
			line1: plan ? editionName(plan.edition) : 'Your edition',
			chip: chipFor(status),
			line2: busy ?? facts,
			warnings,
			action: 'Change',
			disc: {
				tone: busy
					? 'busy'
					: complete && status.state !== 'unconfirmed'
						? 'accent'
						: plan || status.state === 'unconfirmed'
							? 'warning'
							: 'none',
				fill: total ? kept / total : 0,
				spinning: Boolean(busy)
			}
		};
	} else {
		// A person who chose only the group left the files unmanaged on
		// purpose; an automatic group-only match is waiting for its edition.
		const groupOnly = status.edition_kind === 'group_only';
		const line2 = groupOnly
			? status.state === 'chosen'
				? 'Left unmanaged'
				: 'Files kept as they are'
			: status.state === 'unmatched'
				? 'Nothing on MusicBrainz fits it yet'
				: status.reason.code === 'FOLDER_KEEPS_TAGS'
					? 'This folder keeps its own tags'
					: "DroppedNeedle hasn't matched it yet";
		copy = {
			line1: groupOnly && status.state !== 'chosen' ? 'Edition not chosen yet' : 'No edition yet',
			chip: null,
			line2: busy ?? line2,
			warnings: [],
			action: 'Choose',
			disc: { tone: busy ? 'busy' : 'none', fill: 0, spinning: Boolean(busy) }
		};
	}
	const line2 = [copy.line2, ...copy.warnings].filter(Boolean).join(' · ');
	return {
		...copy,
		label: `Edition: ${copy.line1}. ${line2}. ${copy.action} edition`
	};
}

export function moreCopies(n: number): string {
	return `and ${n} more ${n === 1 ? 'copy' : 'copies'}`;
}

// The same sentences as the server's 409 while the files are still placed.
export function placingSentence(attempts: number): string {
	return attempts > 0
		? 'Still matching this album to the edition you chose. MusicBrainz is not answering, so this waits until it does.'
		: 'Still matching this album to the edition you chose. Try again in a moment.';
}

/** The Files row of Next steps once the files are placed. */
export function filesPlacedSentence(kept: number, notKept: number): string {
	const placed =
		kept === 1 ? '1 of your files sits on its tracks.' : `${kept} of your files sit on its tracks.`;
	if (!notKept) return placed;
	const rest =
		notKept === 1
			? "1 file isn't on it and keeps its own tags."
			: `${notKept} files aren't on it and keep their own tags.`;
	return `${placed} ${rest}`;
}

export type TagsMode = 'held' | 'writing' | 'written' | 'manual';

/** The Tags row, except 'written', whose "History" is a link. */
export function tagsSentence(mode: Exclude<TagsMode, 'written'>): string {
	switch (mode) {
		case 'held':
			return 'This album is left unmanaged, so its files keep their tags until you choose "Manage it again".';
		case 'writing':
			return "Writing this edition's tags and file names...";
		case 'manual':
			return 'Your files still carry their old tags. Apps like Subsonic and Jellyfin read the tags, so they show the new edition once you write them.';
	}
}

const TAG_LABELS: Record<string, string> = {
	album: 'Album',
	album_artist: 'Album artist',
	title: 'Title',
	artist: 'Artist',
	track_number: 'Track',
	disc_number: 'Disc',
	date: 'Date',
	label: 'Label',
	catalog_number: 'Catalog number',
	barcode: 'Barcode',
	musicbrainz_ids: 'MusicBrainz IDs'
};

/**
 * One field of a retag preview: its label, the old value and the new one.
 * The MusicBrainz IDs are one summary line, with no old value.
 */
export function tagChange(change: TagChange): { label: string; from: string | null; to: string } {
	const label = TAG_LABELS[change.field] ?? change.field.replaceAll('_', ' ');
	const to = change.to ?? 'removed';
	if (change.field === 'musicbrainz_ids') return { label, from: null, to };
	return { label, from: change.from ?? 'none', to };
}

/** What else a tag write does to a file: its cover art, files it adds, tags it removes. */
export function fileExtras(file: TagWriteFile): { label: string; value: string }[] {
	const extras: { label: string; value: string }[] = [];
	if (file.artwork_changed) extras.push({ label: 'Cover art', value: 'replaced' });
	if (file.added_files.length) {
		extras.push({
			label: 'Adds',
			value: file.added_files.map((path) => path.slice(path.lastIndexOf('/') + 1)).join(', ')
		});
	}
	if (file.removed_tags.length) {
		extras.push({ label: 'Removes', value: file.removed_tags.join(', ') });
	}
	return extras;
}

/** A tag write that is not done yet: what it waits on. */
export function tagWriteBusySentence(step: TagWriteStep, undo: boolean): string {
	if (step === 'writing') return undo ? 'Putting the old tags back...' : 'Writing tags...';
	if (step === 'planning') return 'Reading your files...';
	return 'Waiting for other Library Management work to finish...';
}

/** A server reason with its suggested action. */
export function reasonSentence(reason: Reason): string {
	return reason.action ? `${reason.message} ${reason.action}` : reason.message;
}

export function alreadyTaggedSentence(n: number): string {
	return n === 1 ? '1 file already carries these tags.' : `${n} files already carry these tags.`;
}

export function writeTagsLabel(n: number): string {
	return `Write tags to ${count(n, 'file', 'files')}`;
}

/** A refusal as the server put it, with its suggested action. */
export function failureSentence(error: unknown, fallback: string): string {
	if (error instanceof ApiError) {
		const action = (error.details as { action?: unknown } | null)?.action;
		return typeof action === 'string' && action ? `${error.message} ${action}` : error.message;
	}
	return error instanceof Error ? error.message : fallback;
}

export function extrasButton(n: number): string {
	return `Move ${count(n, 'file', 'files')} not on this edition to the recycle bin`;
}

export function putBackButton(n: number): string {
	return `Put back ${n} removed ${n === 1 ? 'file' : 'files'}`;
}

export function otherAlbumSentence(title: string, artist: string): string {
	return `This edition belongs to another album on MusicBrainz: ${title} by ${artist}. Your copy moves to that album.`;
}

export const MUSICBRAINZ_DOWN =
	"MusicBrainz isn't answering, so this edition can't be compared with your files right now.";

/** "Make your copy this edition": the download that fills its gaps. */
export function getMissingLabel(n: number): string {
	return `Get the ${count(n, 'missing track', 'missing tracks')}`;
}

export function replaceVersionsLabel(n: number): string {
	return `Replace ${count(n, 'track', 'tracks')} you have in another version`;
}

export const GET_MISSING_HELP =
	'One download for the album, checked against this edition. Tracks below your quality cutoff come along and are replaced only by a better file.';

export const REPLACE_VERSIONS_HELP =
	'Your version goes to the recycle bin. It is replaced only by a file of the same quality or better.';

// A download for the edition always asks for its missing tracks, so a
// replacement can only come along with them.
export const REPLACE_NEEDS_MISSING_HELP =
	'Your versions are replaced in the download of the missing tracks, so this needs that on.';

export const REPLACE_ALSO_GETS_MISSING =
	'Replacing your versions also gets the missing tracks, in the same download.';

// A request made with no source waits for one, never refused.
export const NO_SOURCE_SENTENCE =
	'No download source is set up yet, so the download waits and starts by itself once Soulseek, Usenet or a plugin source is saved.';

/** "None of these is right" (admins): what each way out does, shown before the click. */
export function customEditionSentence(album: string): string {
	return `DroppedNeedle manages your files as they are now, under ${album}.`;
}

export const LEAVE_UNMANAGED_SENTENCE =
	'DroppedNeedle stops retagging and renaming these files and drops the edition it guessed. Choose "Manage it again" on the album page to undo this.';

export const SHEET_DESCRIPTION = 'Pick the release your copy is, or the one you want it to become.';

// The app stepped back from the album by itself; choosing an edition
// does not hand it back.
export const LEFT_UNMANAGED_NOTE =
	'This album is left unmanaged. Choosing an edition does not hand it back: DroppedNeedle leaves its files alone until you choose "Manage it again". Writing the edition\'s tags to the files overwrites the changes made outside DroppedNeedle.';

// Matching one file to an edition track by hand (curators, on the album's
// own edition).
export const MATCH_TO_TRACK = 'Match to a track';
export const MATCH_PICKER_LABEL = 'Track on this edition';
export const MATCH = 'Match';
export const ITS_THIS_TRACK = "It's this track";
export const MATCHED_BY_HAND = 'Matched by hand';
export const UNDO_MATCH = 'Undo match';
export const MATCH_MAKES_IT_YOURS = 'Matching a file makes this edition your choice.';
export const MATCH_FAILED = "This file couldn't be matched.";
export const UNMATCH_FAILED = "The match couldn't be undone.";
// A match refused because the album changed under a stale plan; the plan
// is read again, so trying again works.
export const MATCH_STALE = 'This album just changed. Try again.';
// A match the app has written since: its undo waits for that write's undo.
// "History" is a link for admins.
export const MATCH_WRITTEN_PARTS = ['Tags written. Undo that in ', 'History', ' first.'] as const;
export const MATCH_WRITTEN = MATCH_WRITTEN_PARTS.join('');

/** A row's button, named with the file it acts on. */
export function forFile(label: string, title: string): string {
	return `${label}, ${title}`;
}

/** A free edition track in the picker: `1.3 Title (4:05)`. */
export function freeTrackLabel(track: {
	disc_number: number;
	position: number;
	title: string;
	length_ms?: number | null;
}): string {
	const length = track.length_ms ? ` (${formatDuration(track.length_ms)})` : '';
	return `${track.disc_number}.${track.position} ${track.title}${length}`;
}
