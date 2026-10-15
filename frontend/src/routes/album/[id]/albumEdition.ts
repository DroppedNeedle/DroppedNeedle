// What the album's current edition plan makes of its tracks and files, for
// the album pages' track rows and their "Unmatched files" section.
import type { EditionPlan } from '$lib/queries/albums/EditionQueries.svelte';
import { getDiscTrackKey } from '$lib/player/queueHelpers';
import type { LibraryFileMeta } from '$lib/types';

type EditionPlanFile = EditionPlan['files'][number];

/** What the album's current edition makes of one of its tracks. */
export interface EditionTrackRow {
	/** No file sits on the track. */
	missing: boolean;
	/** The library's facts about the file that sits on the track. */
	file: LibraryFileMeta | null;
	/** The file's length when one sits on the track, else the edition's. */
	lengthMs: number | null;
	/** The length of a file that is another version of this missing track. */
	yourVersionMs: number | null;
	note: string | null;
}

/** A file under the album that sits on none of its edition's tracks. */
export interface UnmatchedFile {
	id: string;
	title: string;
	artistName: string | null;
	format: string | null;
	lengthMs: number | null;
	label: string;
	note: string | null;
}

const FILE_NOTES: Record<string, string> = {
	TRACK_ID_CORRECTED:
		"This file's track id belonged to another edition; it will be corrected when tags are written",
	TRACK_ID_ELSEWHERE: "This file's track id belongs to another edition of this album"
};

export function fileNote(file: Pick<EditionPlanFile, 'note'>): string | null {
	if (!file.note) return null;
	return FILE_NOTES[file.note.code] ?? file.note.message;
}

/** The plan's sentence about each file that has one, by track id. */
export function notesByFile(plan: EditionPlan): Map<string, string> {
	const notes = new Map<string, string>();
	for (const file of plan.files) {
		const note = fileNote(file);
		if (note) notes.set(file.local_track_id, note);
	}
	return notes;
}

/** Each edition track by disc and position, as the plan lines the files up on it. */
export function editionTrackRows(
	plan: EditionPlan,
	filesById: Map<string, LibraryFileMeta>
): Map<string, EditionTrackRow> {
	const planFiles = new Map(plan.files.map((file) => [file.local_track_id, file]));
	const versions = new Map<string, EditionPlanFile>();
	for (const file of plan.files) {
		if (file.fate !== 'other_version' || file.edition_position == null) continue;
		versions.set(
			getDiscTrackKey({ disc_number: file.edition_disc_number, position: file.edition_position }),
			file
		);
	}
	const rows = new Map<string, EditionTrackRow>();
	for (const track of plan.tracks) {
		const key = getDiscTrackKey(track);
		const planFile = track.local_track_id ? planFiles.get(track.local_track_id) : undefined;
		const version = track.local_track_id ? undefined : versions.get(key);
		rows.set(key, {
			missing: !track.local_track_id,
			file: track.local_track_id ? (filesById.get(track.local_track_id) ?? null) : null,
			lengthMs: planFile?.length_ms ?? track.length_ms ?? null,
			yourVersionMs: version ? (version.length_ms ?? null) : null,
			note: planFile ? fileNote(planFile) : null
		});
	}
	return rows;
}

function trackName(file: EditionPlanFile): string {
	const disc = file.edition_disc_number ?? 1;
	return disc > 1
		? `disc ${disc} track ${file.edition_position}`
		: `track ${file.edition_position}`;
}

function unmatchedLabel(file: EditionPlanFile): string {
	if (file.edition_position != null) {
		if (file.fate === 'other_version') return `Another version of ${trackName(file)}`;
		if (file.fate === 'duplicate') return `Second copy of ${trackName(file)}`;
	}
	return 'Not on this edition';
}

/** The album's files the edition has no track for, each labelled with why. */
export function unmatchedFiles(
	plan: EditionPlan,
	filesById: Map<string, LibraryFileMeta>
): UnmatchedFile[] {
	return plan.files
		.filter((file) => file.fate !== 'keep')
		.map((file) => {
			const meta = filesById.get(file.local_track_id);
			return {
				id: file.local_track_id,
				title: file.title,
				artistName: meta?.artist_name ?? null,
				format: meta?.format ?? null,
				lengthMs: file.length_ms ?? null,
				label: unmatchedLabel(file),
				note: fileNote(file)
			};
		});
}
