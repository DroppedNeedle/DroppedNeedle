import { describe, expect, it } from 'vitest';

import type { components } from '$lib/api/v3/openapi';
import {
	MATCH_MAKES_IT_YOURS,
	MATCH_WRITTEN,
	filesPlacedSentence,
	fitLine,
	formatsLabel,
	freeTrackLabel,
	plateCopy,
	summarySentence
} from './editionCopy';

type EditionStatus = components['schemas']['EditionStatusView'];
type EditionPlan = components['schemas']['EditionPlanView'];

function status(overrides: Partial<EditionStatus> = {}): EditionStatus {
	return {
		album_id: 'album-1',
		candidates: [],
		edition_kind: 'release',
		files_by_itself: true,
		keeps_own_tags: false,
		identifying: false,
		identity_revision: 3,
		placing: null,
		reason: { code: 'SUPPORTED', message: 'Fits.', action: '' },
		release_group_mbid: 'group-1',
		release_mbid: 'release-1',
		state: 'chosen',
		undo_available: false,
		writing: false,
		...overrides
	} as EditionStatus;
}

function plan(counts: {
	files: number;
	kept: number;
	tracks?: number;
	missing?: number;
	versions?: number;
	extra?: number;
	duplicate?: number;
}): EditionPlan {
	const tracks = counts.tracks ?? counts.files;
	return {
		album_id: 'album-1',
		artist_name: 'The Lanterns',
		duplicate_count: counts.duplicate ?? 0,
		edition: {
			title: 'Night Shift',
			disambiguation: '5th anniversary remaster',
			date: '2024-03-01',
			country: 'XW',
			formats: ['Digital Media'],
			disc_count: 1,
			track_count: tracks
		},
		extra_count: counts.extra ?? 0,
		files: Array.from({ length: counts.files }, (_, i) => ({
			disc_number: 1,
			track_number: i + 1,
			fate: 'keep',
			local_track_id: `file-${i}`,
			title: `Song ${i}`
		})),
		is_current: true,
		kept_count: counts.kept,
		missing_count: counts.missing ?? 0,
		other_version_count: counts.versions ?? 0,
		release_group_mbid: 'group-1',
		release_mbid: 'release-1',
		same_album: true,
		title: 'Night Shift',
		tracks: Array.from({ length: tracks }, (_, i) => ({
			disc_number: 1,
			position: i + 1,
			recording_mbid: `rec-${i}`,
			release_track_mbid: `track-${i}`,
			title: `Song ${i}`
		}))
	} as EditionPlan;
}

describe('the edition plate', () => {
	it('names a chosen edition and counts the tracks the files hold', () => {
		const copy = plateCopy(
			status(),
			plan({ files: 4, kept: 3, tracks: 4, missing: 1, versions: 1 }),
			4
		);
		expect(copy.line1).toBe('5th anniversary remaster');
		expect(copy.chip?.label).toBe('Your choice');
		expect(copy.line2).toBe('Released 2024 · XW · Digital Media · 3 of 4 tracks');
		expect(copy.warnings).toEqual(['1 in another version']);
		expect(copy.label).toBe(
			'Edition: 5th anniversary remaster. Released 2024 · XW · Digital Media · 3 of 4 tracks · 1 in another version. Change edition'
		);
		expect(copy.disc.tone).toBe('warning');
	});

	it('says who picked it', () => {
		expect(plateCopy(status({ state: 'confirmed' }), null, 4).chip?.label).toBe(
			'Picked by DroppedNeedle'
		);
		const guess = plateCopy(status({ state: 'unconfirmed' }), plan({ files: 4, kept: 4 }), 4);
		expect(guess.chip?.label).toBe('Best guess');
		expect(guess.disc.tone).toBe('warning');
	});

	it('falls back to the first format when the edition has no disambiguation', () => {
		const cd = plan({ files: 2, kept: 2 });
		cd.edition.disambiguation = null;
		cd.edition.formats = ['CD', 'CD'];
		const copy = plateCopy(status({ state: 'confirmed' }), cd, 2);
		expect(copy.line1).toBe('CD edition');
		expect(copy.line2).toBe('Released 2024 · XW · 2×CD · 2 of 2 tracks');
		expect(copy.disc.tone).toBe('accent');
	});

	it('shows a custom edition as the files are', () => {
		const copy = plateCopy(status({ edition_kind: 'custom', release_mbid: null }), null, 7);
		expect(copy.line1).toBe('Your own track list');
		expect(copy.line2).toBe('Kept as your files are · 7 tracks');
	});

	it('says while the files are matched or written', () => {
		expect(plateCopy(status({ placing: { attempts: 0 } }), null, 4).line2).toBe(
			'Matching your files to it...'
		);
		const waiting = plateCopy(status({ placing: { attempts: 2 } }), null, 4);
		expect(waiting.line2).toBe('Waiting for MusicBrainz');
		expect(waiting.disc).toMatchObject({ tone: 'busy', spinning: true });
		expect(plateCopy(status({ writing: true }), null, 4).line2).toBe(
			'Writing tags and file names...'
		);
	});

	it('gives the reason an album has no edition yet', () => {
		const none = { edition_kind: 'none' as const, release_mbid: null, release_group_mbid: null };
		const unidentified = plateCopy(status({ ...none, state: 'unidentified' }), null, 4);
		expect(unidentified.line1).toBe('No edition yet');
		expect(unidentified.line2).toBe("DroppedNeedle hasn't matched it yet");
		expect(unidentified.action).toBe('Choose');
		expect(
			plateCopy(
				status({
					...none,
					state: 'unidentified',
					reason: { code: 'FOLDER_KEEPS_TAGS', message: '', action: '' }
				}),
				null,
				4
			).line2
		).toBe('This folder keeps its own tags');
		expect(plateCopy(status({ ...none, state: 'unmatched' }), null, 4).line2).toBe(
			'Nothing on MusicBrainz fits it yet'
		);
		const groupOnly = { edition_kind: 'group_only' as const, release_mbid: null };
		const automatic = plateCopy(status({ ...groupOnly, state: 'confirmed' }), null, 4);
		expect(automatic.line1).toBe('Edition not chosen yet');
		expect(automatic.line2).toBe('Files kept as they are');
		const chosen = plateCopy(status({ ...groupOnly, state: 'chosen' }), null, 4);
		expect(chosen.line1).toBe('No edition yet');
		expect(chosen.line2).toBe('Left unmanaged');
	});
});

describe('edition sentences', () => {
	it('counts the fit of a list row', () => {
		expect(fitLine(4, 4)).toBe('Fits all 4 of your files');
		expect(fitLine(3, 4)).toBe('Fits 3 of your 4 files');
		expect(fitLine(0, 4)).toBe('Fits none of your files');
		expect(fitLine(1, 1)).toBe('Fits your 1 file');
	});

	it('sums up the plan with each plural written out', () => {
		expect(summarySentence(plan({ files: 4, kept: 4 }))).toBe(
			'All 4 of your files fit this edition.'
		);
		expect(summarySentence(plan({ files: 4, kept: 3, tracks: 4, missing: 1, versions: 1 }))).toBe(
			'3 of your 4 files fit this edition. 1 of your files is another version of a track on it.'
		);
		expect(
			summarySentence(plan({ files: 6, kept: 2, tracks: 5, missing: 3, extra: 2, duplicate: 2 }))
		).toBe(
			"2 of your 6 files fit this edition. 3 of its tracks are missing. 2 of your files aren't on it. 2 are second copies."
		);
		expect(summarySentence(plan({ files: 3, kept: 2, extra: 1, duplicate: 1 }))).toBe(
			"2 of your 3 files fit this edition. 1 of your files isn't on it. 1 is a second copy."
		);
		expect(summarySentence(plan({ files: 3, kept: 0 }))).toBe(
			'None of your files fit this edition, so it is probably a different album.'
		);
	});

	it('says where the files went after a choice', () => {
		expect(filesPlacedSentence(3, 1)).toBe(
			"3 of your files sit on its tracks. 1 file isn't on it and keeps its own tags."
		);
		expect(filesPlacedSentence(1, 2)).toBe(
			"1 of your files sits on its tracks. 2 files aren't on it and keep their own tags."
		);
	});

	it('names a free track in the match picker and says what a match does', () => {
		expect(
			freeTrackLabel({ disc_number: 1, position: 3, title: 'Lantern Light', length_ms: 245_000 })
		).toBe('1.3 Lantern Light (4:05)');
		expect(freeTrackLabel({ disc_number: 2, position: 1, title: 'Coda', length_ms: null })).toBe(
			'2.1 Coda'
		);
		expect(MATCH_MAKES_IT_YOURS).toBe('Matching a file makes this edition your choice.');
		expect(MATCH_WRITTEN).toBe('Tags written. Undo that in History first.');
	});

	it('folds repeated formats', () => {
		expect(formatsLabel(['CD', 'CD', 'DVD'])).toBe('2×CD + DVD');
		expect(formatsLabel([])).toBe('');
	});
});
