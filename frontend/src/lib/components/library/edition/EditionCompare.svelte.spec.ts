import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import { ApiError } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';

type EditionPlan = components['schemas']['EditionPlanView'];

import '../../../../app.css';
import EditionCompare from './EditionCompare.svelte';
import EditionDetail from './EditionDetail.svelte';

const track = (position: number, title: string, length: number, file: string | null) => ({
	disc_number: 1,
	position,
	title,
	length_ms: length,
	recording_mbid: `rec-${position}`,
	release_track_mbid: `track-${position}`,
	local_track_id: file
});

const file = (
	id: string,
	title: string,
	fate: string,
	at: number | null = null,
	matchedByHand = false
) => ({
	local_track_id: id,
	title,
	disc_number: 1,
	track_number: at ?? 9,
	length_ms: 200_000,
	fate,
	edition_disc_number: at === null ? null : 1,
	edition_position: at,
	note: null,
	matched_by_hand: matchedByHand,
	match_written: false
});

// 1.1 fits; 1.2 is held in another version; 1.3 is missing; 1.4 holds a
// file the person matched by hand. A file not on the edition and a second
// copy follow.
const plan = {
	album_id: 'local-1',
	release_group_mbid: 'group-1',
	release_mbid: 'release-1',
	title: 'Night Shift',
	artist_name: 'The Lanterns',
	is_current: true,
	same_album: true,
	identity_revision: 7,
	edition: {
		title: 'Night Shift',
		disambiguation: null,
		date: '2024-03-01',
		country: 'XW',
		formats: ['CD'],
		disc_count: 1,
		track_count: 4
	},
	kept_count: 2,
	missing_count: 2,
	duplicate_count: 1,
	other_version_count: 1,
	extra_count: 1,
	tracks: [
		track(1, 'Lamplight', 181_000, 'file-1'),
		track(2, 'Blue Hour', 180_000, null),
		track(3, 'Night Shift', 245_000, null),
		track(4, 'Coda', 120_000, 'file-4')
	],
	files: [
		file('file-1', 'Lamplight', 'keep', 1),
		file('file-2', 'Blue Hour (live)', 'other_version', 2),
		file('file-x', 'Untitled Take', 'extra'),
		file('file-d', 'Lamplight (copy)', 'duplicate', 1),
		file('file-4', 'Coda', 'keep', 4, true)
	]
} as unknown as EditionPlan;

const h = vi.hoisted(() => ({ match: vi.fn(), unmatch: vi.fn(), isAdmin: false }));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: {
		user: { id: 'user-1' },
		get isAdmin() {
			return h.isAdmin;
		}
	},
	LAST_USER_ID_KEY: 'test:last-user'
}));

const row = (text: string) => page.getByRole('row').filter({ hasText: text });
// The picker's Match button, whatever else its name says.
const matchButton = () => page.getByRole('button', { name: /^Match(,|$)/ });

beforeEach(() => {
	h.match.mockReset().mockResolvedValue(undefined);
	h.unmatch.mockReset().mockResolvedValue(undefined);
	h.isAdmin = false;
});

function compare(appPicked = false, shown: EditionPlan = plan) {
	return render(EditionCompare, {
		plan: shown,
		canMatch: true,
		appPicked,
		onmatch: h.match,
		onunmatch: h.unmatch
	});
}

describe('matching a file by hand', () => {
	it('lists only the free tracks and matches the picked one', async () => {
		await compare();
		await row('Untitled Take').getByRole('button', { name: 'Match to a track' }).click();
		const picker = page.getByLabelText('Track on this edition');
		await expect.element(picker).toBeVisible();
		const options = picker.getByRole('option');
		await expect.element(options.nth(0)).toHaveTextContent('1.2 Blue Hour (3:00)');
		await expect.element(options.nth(1)).toHaveTextContent('1.3 Night Shift (4:05)');
		expect(options.all()).toHaveLength(2);
		await picker.selectOptions('track-3');
		await matchButton().click();
		expect(h.match).toHaveBeenCalledWith('file-x', 'track-3', 7);
		await expect.element(page.getByLabelText('Track on this edition')).not.toBeInTheDocument();
	});

	it('offers a second copy the picker too', async () => {
		await compare();
		await expect
			.element(row('Lamplight (copy)').getByRole('button', { name: 'Match to a track' }))
			.toBeVisible();
	});

	it('matches another version to its track in one click', async () => {
		await compare();
		await row('Blue Hour (live)').getByRole('button', { name: "It's this track" }).click();
		expect(h.match).toHaveBeenCalledWith('file-2', 'track-2', 7);
	});

	it('shows a match by hand with its undo', async () => {
		await compare();
		const coda = row('Coda');
		await expect.element(coda.getByText('Matched by hand')).toBeVisible();
		await coda.getByRole('button', { name: 'Undo match' }).click();
		expect(h.unmatch).toHaveBeenCalledWith('file-4');
		await expect
			.element(row('Lamplight').first().getByRole('button', { name: 'Undo match' }))
			.not.toBeInTheDocument();
	});

	it("shows a refusal on the file's row and keeps the picker open", async () => {
		h.match.mockRejectedValue(new Error('Another file is on that track now.'));
		await compare();
		await row('Untitled Take').getByRole('button', { name: 'Match to a track' }).click();
		await matchButton().click();
		await expect
			.element(row('Untitled Take').getByRole('alert'))
			.toHaveTextContent('Another file is on that track now.');
		await expect.element(page.getByLabelText('Track on this edition')).toBeVisible();
	});

	it('keeps a refusal and the picker when the plan is read again, and the retry works', async () => {
		h.match.mockRejectedValueOnce(new ApiError(409, 'The album changed.', 'STALE_REVISION'));
		const view = await compare();
		await row('Untitled Take').getByRole('button', { name: 'Match to a track' }).click();
		await page.getByLabelText('Track on this edition').selectOptions('track-3');
		await matchButton().click();
		const alert = row('Untitled Take').getByRole('alert');
		await expect.element(alert).toHaveTextContent('This album just changed. Try again.');

		// Meanwhile another file was matched to 1.3.
		await view.rerender({
			plan: {
				...plan,
				identity_revision: 8,
				tracks: plan.tracks.map((track) =>
					track.position === 3 ? { ...track, local_track_id: 'file-y' } : track
				),
				files: [...plan.files, file('file-y', 'Night Shift (take 2)', 'keep', 3, true)]
			} as EditionPlan
		});
		await expect.element(alert).toHaveTextContent('This album just changed. Try again.');
		const picker = page.getByLabelText('Track on this edition');
		await expect.element(picker).toHaveValue('track-2');
		expect(picker.getByRole('option').all()).toHaveLength(1);
		await matchButton().click();
		expect(h.match).toHaveBeenLastCalledWith('file-x', 'track-2', 8);
	});

	it('starts the picker on the closest length no other version sits beside', async () => {
		await compare(false, {
			...plan,
			tracks: [...plan.tracks, track(5, 'Harbour Lights', 150_000, null)],
			files: plan.files.map((one) =>
				one.local_track_id === 'file-x' ? { ...one, length_ms: 181_000 } : one
			)
		} as EditionPlan);
		await row('Untitled Take').getByRole('button', { name: 'Match to a track' }).click();
		await expect.element(page.getByLabelText('Track on this edition')).toHaveValue('track-5');
	});

	it('moves focus to the picker, and back to the row on Cancel', async () => {
		await compare();
		await row('Untitled Take')
			.getByRole('button', { name: 'Match to a track, Untitled Take', exact: true })
			.click();
		await expect.element(page.getByLabelText('Track on this edition')).toHaveFocus();
		await page.getByRole('button', { name: 'Cancel' }).click();
		await expect
			.element(row('Untitled Take').getByRole('button', { name: 'Match to a track' }))
			.toHaveFocus();
	});

	it('holds back the undo once the file is written, linking History for admins', async () => {
		const written = {
			...plan,
			files: plan.files.map((one) =>
				one.local_track_id === 'file-4' ? { ...one, match_written: true } : one
			)
		} as EditionPlan;
		const view = await compare(false, written);
		const coda = row('Coda');
		await expect.element(coda.getByText('Matched by hand')).toBeVisible();
		await expect.element(coda.getByText('Tags written. Undo that in History first.')).toBeVisible();
		await expect.element(coda.getByRole('button', { name: /Undo match/ })).not.toBeInTheDocument();
		await expect.element(coda.getByRole('link', { name: 'History' })).not.toBeInTheDocument();
		view.unmount();

		h.isAdmin = true;
		await compare(false, written);
		await expect.element(row('Coda').getByRole('link', { name: 'History' })).toBeVisible();
	});

	it('says once, above the table, that a match makes the edition yours when the app picked it', async () => {
		const line = 'Matching a file makes this edition your choice.';
		const { unmount } = await compare(true);
		await row('Untitled Take').getByRole('button', { name: 'Match to a track' }).click();
		await expect.element(page.getByText(line)).toBeVisible();
		expect(page.getByText(line).all()).toHaveLength(1);
		expect(page.getByRole('row').filter({ hasText: line }).all()).toHaveLength(0);
		unmount();

		await compare(false);
		await row('Untitled Take').getByRole('button', { name: 'Match to a track' }).click();
		await expect.element(page.getByText(line)).not.toBeInTheDocument();
	});
});

describe('who may match', () => {
	function detail(overrides: { isCurrent?: boolean; canEdit?: boolean; editionKind?: string }) {
		return render(EditionDetail, {
			release: 'release-1',
			query: {
				data: plan,
				isPending: false,
				isError: false,
				error: null,
				refetch: () => undefined
			},
			isCurrent: overrides.isCurrent ?? true,
			hiddenByFilter: false,
			albumHasGroup: true,
			canEdit: overrides.canEdit ?? true,
			actionError: null,
			putBackCount: null,
			onremoveextras: vi.fn(),
			onputback: vi.fn(),
			sourceReady: true,
			getMissing: true,
			replaceVersions: true,
			group: '',
			tracks: null,
			onacquire: vi.fn(),
			editionKind: overrides.editionKind ?? 'release',
			appPicked: false,
			onmatch: h.match,
			onunmatch: h.unmatch
		});
	}

	it("offers it to a curator on the album's own edition", async () => {
		await detail({});
		await expect
			.element(row('Untitled Take').getByRole('button', { name: 'Match to a track' }))
			.toBeVisible();
	});

	for (const [who, overrides] of [
		['another edition', { isCurrent: false }],
		['a plain user', { canEdit: false }],
		['a custom edition', { editionKind: 'custom' }]
	] as const) {
		it(`shows none of it for ${who}`, async () => {
			await detail(overrides);
			await expect.element(row('Untitled Take')).toBeVisible();
			await expect
				.element(page.getByRole('button', { name: 'Match to a track' }))
				.not.toBeInTheDocument();
			await expect
				.element(page.getByRole('button', { name: "It's this track" }))
				.not.toBeInTheDocument();
			await expect.element(page.getByText('Matched by hand')).not.toBeInTheDocument();
			await expect
				.element(page.getByRole('button', { name: 'Undo match' }))
				.not.toBeInTheDocument();
		});
	}
});
