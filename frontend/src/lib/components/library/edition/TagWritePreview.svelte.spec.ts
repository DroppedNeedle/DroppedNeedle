import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { components } from '$lib/api/v3/openapi';

type EditionStatus = components['schemas']['EditionStatusView'];
type EditionChoice = components['schemas']['EditionChoiceView'];

const h = vi.hoisted(() => ({
	preview: vi.fn(),
	write: vi.fn(),
	undo: vi.fn()
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	// A curator, not an admin: whoever wrote the tags may undo them.
	authStore: { user: { id: 'user-1' }, isAdmin: false, isTrusted: true },
	LAST_USER_ID_KEY: 'test:last-user'
}));
vi.mock('$lib/queries/albums/EditionQueries.svelte', () => ({
	previewEditionTags: () => ({ mutateAsync: h.preview, isPending: false }),
	writeEditionTags: () => ({ mutateAsync: h.write, isPending: false }),
	undoTagWrite: () => ({ mutateAsync: h.undo, isPending: false })
}));

import '../../../../app.css';
import EditionNextSteps from './EditionNextSteps.svelte';

const album = { userId: 'user-1', localId: 'local-1', rgMbid: 'group-1' };

function status(overrides: Partial<EditionStatus> = {}): EditionStatus {
	return {
		album_id: 'local-1',
		candidates: [],
		edition_kind: 'release',
		files_by_itself: false,
		identifying: false,
		identity_revision: 2,
		placing: null,
		reason: { code: 'SUPPORTED', message: 'The files fit this edition.', action: '' },
		release_group_mbid: 'group-1',
		release_mbid: 'release-remaster',
		state: 'chosen',
		undo_available: true,
		writing: false,
		...overrides
	} as EditionStatus;
}

const files = ['01 Lamplight.flac', '02 Blue Hour.flac', '03 Night Shift.flac'].map((rel_path) => ({
	root_id: 'root-1',
	rel_path
}));

const choice = {
	album_id: 'local-1',
	extra_track_ids: [],
	missing_titles: [],
	release_group_mbid: 'group-1',
	release_mbid: 'release-remaster',
	retag_files: files,
	status: status()
} as unknown as EditionChoice;

const previewAnswer = {
	bundle_id: 'bundle-preview',
	expires_day: 20400,
	preview_token: 'token-1',
	files: [
		{
			track_id: 'track-1',
			kind: 'same_path',
			source: 'root-1/01 Lamplight.flac',
			dest: 'root-1/01 Lamplight.flac',
			changes: [
				{ field: 'album', from: 'Night Shift', to: 'Night Shift (5th anniversary remaster)' },
				{ field: 'musicbrainz_ids', from: null, to: '4 changed, 2 kept' }
			]
		},
		...['02 Blue Hour.flac', '03 Night Shift.flac'].map((name, i) => ({
			track_id: `track-${i + 2}`,
			kind: 'same_path',
			source: `root-1/${name}`,
			dest: `root-1/${name}`,
			changes: []
		}))
	]
};

function show(edition: EditionStatus = status()) {
	return render(EditionNextSteps, {
		name: '5th anniversary remaster',
		status: edition,
		plan: null,
		choice,
		held: false,
		error: null,
		onundo: vi.fn(),
		album,
		group: '',
		gaps: { missing: 0, versions: 0 },
		sourceReady: true,
		tracks: null,
		onacquire: vi.fn()
	});
}

beforeEach(() => {
	h.preview.mockReset().mockResolvedValue(previewAnswer);
	h.write.mockReset().mockResolvedValue({ bundle_id: 'bundle-1', files: [], outcome: 'committed' });
	h.undo
		.mockReset()
		.mockResolvedValue({ bundle_id: 'bundle-2', outcome: 'committed', restored: [] });
});

describe("writing the edition's tags in a folder the app does not write by itself", () => {
	it('previews each change and writes nothing until "Write tags"', async () => {
		show();
		await page.getByRole('button', { name: 'Preview the new tags' }).click();

		const list = page.getByRole('list', { name: 'Tags that change' });
		await expect
			.element(list.getByText('Album: Night Shift -> Night Shift (5th anniversary remaster)'))
			.toBeVisible();
		await expect.element(list.getByText('MusicBrainz IDs: 4 changed, 2 kept')).toBeVisible();
		await expect.element(page.getByText('2 files already carry these tags.')).toBeVisible();
		expect(h.preview).toHaveBeenCalledWith({ ...album, files });
		expect(h.write).not.toHaveBeenCalled();
		await expect.element(page.getByRole('button', { name: 'Write tags to 1 file' })).toBeVisible();
	});

	it('writes with the preview token, then lets the curator undo that write by its bundle id', async () => {
		show();
		await page.getByRole('button', { name: 'Preview the new tags' }).click();
		await page.getByRole('button', { name: 'Write tags to 1 file' }).click();

		await expect.element(page.getByText('Tags written.')).toBeVisible();
		expect(h.write).toHaveBeenCalledWith({ ...album, previewToken: 'token-1' });
		expect(h.undo).not.toHaveBeenCalled();

		await page.getByRole('button', { name: 'Undo', exact: true }).click();
		await expect.element(page.getByText('The old tags are back.')).toBeVisible();
		expect(h.undo).toHaveBeenCalledWith({ ...album, bundleId: 'bundle-1' });
	});

	it('waits while the files are placed, saying why', async () => {
		show(status({ placing: { attempts: 0 } as EditionStatus['placing'] }));
		const button = page.getByRole('button', { name: 'Preview the new tags' });
		await expect.element(button).toBeDisabled();
		await expect
			.element(button)
			.toHaveAccessibleDescription(
				'Still matching this album to the edition you chose. Try again in a moment.'
			);
	});
});
