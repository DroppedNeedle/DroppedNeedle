import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import { ApiError } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import type { LibraryAlbumSummary } from '$lib/types';

type EditionStatus = components['schemas']['EditionStatusView'];
type EditionPlan = components['schemas']['EditionPlanView'];

const h = vi.hoisted(() => ({
	status: undefined as Record<string, unknown> | undefined,
	plans: {} as Record<string, unknown>,
	planErrors: {} as Record<string, unknown>,
	editions: [] as unknown[],
	choose: vi.fn(),
	confirm: vi.fn(),
	isTrusted: true,
	url: new URL('http://localhost/album/group-1'),
	goto: vi.fn()
}));

vi.mock('$app/state', () => ({
	page: {
		get url() {
			return h.url;
		}
	}
}));
vi.mock('$app/navigation', () => ({ goto: h.goto }));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: {
		user: { id: 'user-1' },
		get isTrusted() {
			return h.isTrusted;
		}
	},
	LAST_USER_ID_KEY: 'test:last-user'
}));

const mutation = (mutateAsync: () => unknown = vi.fn()) => ({ mutateAsync, isPending: false });

vi.mock('$lib/queries/albums/EditionQueries.svelte', () => ({
	getAlbumEditionStatusQuery: (_user: () => string, getId: () => string) => ({
		get data() {
			return h.status ? { ...h.status, album_id: getId() } : undefined;
		},
		isPending: false,
		isError: false,
		error: null,
		refetch: vi.fn()
	}),
	getEditionPlanQuery: (
		_user: () => string,
		_id: () => string,
		getRelease: () => string | null
	) => ({
		get data() {
			return h.plans[getRelease() ?? 'current'];
		},
		get error() {
			return h.planErrors[getRelease() ?? 'current'] ?? null;
		},
		get isError() {
			return Boolean(h.planErrors[getRelease() ?? 'current']);
		},
		get isPending() {
			const key = getRelease() ?? 'current';
			return !h.plans[key] && !h.planErrors[key];
		},
		refetch: vi.fn()
	}),
	getAlbumEditionsQuery: () => ({
		get data() {
			return { items: h.editions };
		},
		isLoading: false,
		isError: false
	}),
	chooseAlbumEdition: () => mutation((...args: unknown[]) => h.choose(...args)),
	confirmAlbumEdition: () => mutation((...args: unknown[]) => h.confirm(...args)),
	handBackAlbumEdition: () => mutation(),
	undoAlbumEdition: () => mutation(),
	acquireEdition: () => mutation(),
	retagAfterChoice: () => mutation(),
	removeEditionExtras: () => mutation(),
	restoreEditionExtras: () => mutation()
}));
vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibraryAlbumDetailQuery: () => ({ data: undefined })
}));
vi.mock('$lib/queries/library/LibraryEditionQueries.svelte', () => ({
	getReleaseEditionSearchQuery: () => ({ data: undefined, isError: false, isFetching: false })
}));
vi.mock('$lib/components/AlbumImage.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});

import '../../../../app.css';
import EditionPlate from './EditionPlate.svelte';

const copy = {
	id: 'local-1',
	title: 'Night Shift',
	artist_name: 'The Lanterns',
	track_count: 4,
	format: 'flac'
} as LibraryAlbumSummary;

const minutes = (m: number, s: number) => (m * 60 + s) * 1000;

function status(overrides: Partial<EditionStatus> = {}): EditionStatus {
	return {
		album_id: 'local-1',
		candidates: [],
		edition_kind: 'release',
		files_by_itself: true,
		identifying: false,
		identity_revision: 2,
		placing: null,
		reason: { code: 'SUPPORTED', message: 'The files fit this edition.', action: '' },
		release_group_mbid: 'group-1',
		release_mbid: 'release-cd',
		state: 'confirmed',
		undo_available: false,
		writing: false,
		...overrides
	} as EditionStatus;
}

const titles = ['Lamplight', 'Blue Hour', 'Night Shift', 'Last Train Home'];

function cdPlan(): EditionPlan {
	return {
		album_id: 'local-1',
		artist_name: 'The Lanterns',
		duplicate_count: 0,
		edition: {
			title: 'Night Shift',
			disambiguation: null,
			date: '2019-05-01',
			country: 'GB',
			formats: ['CD'],
			disc_count: 1,
			track_count: 4
		},
		extra_count: 0,
		files: titles.map((title, i) => ({
			disc_number: 1,
			track_number: i + 1,
			edition_disc_number: 1,
			edition_position: i + 1,
			fate: 'keep',
			length_ms: minutes(3, 30 + i),
			local_track_id: `file-${i}`,
			title
		})),
		is_current: true,
		kept_count: 4,
		missing_count: 0,
		other_version_count: 0,
		release_group_mbid: 'group-1',
		release_mbid: 'release-cd',
		same_album: true,
		title: 'Night Shift',
		tracks: titles.map((title, i) => ({
			disc_number: 1,
			position: i + 1,
			length_ms: minutes(3, 30 + i),
			local_track_id: `file-${i}`,
			recording_mbid: `rec-${i}`,
			release_track_mbid: `cd-track-${i}`,
			title
		}))
	} as EditionPlan;
}

// The remaster's Blue Hour is longer: the file is another version of it.
function remasterPlan(): EditionPlan {
	const plan = cdPlan();
	return {
		...plan,
		edition: {
			...plan.edition,
			disambiguation: '5th anniversary remaster',
			date: '2024-03-01',
			country: 'XW',
			formats: ['Digital Media']
		},
		files: plan.files.map((file) =>
			file.title === 'Blue Hour'
				? { ...file, fate: 'other_version', length_ms: minutes(4, 11) }
				: file
		),
		is_current: false,
		kept_count: 3,
		missing_count: 1,
		other_version_count: 1,
		release_mbid: 'release-remaster',
		tracks: plan.tracks.map((track) => ({
			...track,
			release_track_mbid: `re-${track.release_track_mbid}`,
			...(track.title === 'Blue Hour' ? { local_track_id: null, length_ms: minutes(4, 56) } : {})
		}))
	} as EditionPlan;
}

function edition(release: string, date: string, extra: Record<string, unknown> = {}) {
	return {
		release_mbid: release,
		title: 'Night Shift',
		date,
		country: 'GB',
		media_formats: ['CD'],
		track_count: 4,
		is_owned: false,
		is_pinned: false,
		...extra
	};
}

const sheet = () => page.getByRole('dialog', { name: 'Edition of Night Shift' });
const plate = () => page.getByRole('button', { name: /^Edition: / });

async function openSheet(): Promise<void> {
	await render(EditionPlate, { copies: [copy], groupMbid: 'group-1' });
	await plate().click();
	await expect.element(sheet()).toBeVisible();
}

beforeEach(async () => {
	await page.viewport(1280, 900);
	h.status = status();
	h.plans = { current: cdPlan(), 'release-remaster': remasterPlan() };
	h.planErrors = {};
	h.editions = [
		edition('release-cd', '2019-05-01'),
		edition('release-remaster', '2024-03-01', {
			disambiguation: '5th anniversary remaster',
			country: 'XW',
			media_formats: ['Digital Media']
		})
	];
	h.choose.mockReset();
	h.isTrusted = true;
	h.url = new URL('http://localhost/album/group-1');
	h.goto.mockReset();
});

describe('the edition plate', () => {
	it.each([
		[
			'chosen',
			{ state: 'chosen' },
			'CD edition',
			'Your choice',
			'Released 2019 · GB · CD · 4 of 4 tracks'
		],
		['automatic', {}, 'CD edition', 'Picked by DroppedNeedle', '4 of 4 tracks'],
		['a best guess', { state: 'unconfirmed' }, 'CD edition', 'Best guess', '4 of 4 tracks'],
		[
			'custom',
			{ edition_kind: 'custom', release_mbid: null, state: 'chosen' },
			'Your own track list',
			'Your choice',
			'Kept as your files are · 4 tracks'
		],
		['placing', { placing: { attempts: 0 } }, 'CD edition', null, 'Matching your files to it...'],
		['waiting', { placing: { attempts: 3 } }, 'CD edition', null, 'Waiting for MusicBrainz'],
		[
			'unidentified',
			{ edition_kind: 'none', release_mbid: null, state: 'unidentified' },
			'No edition yet',
			null,
			"DroppedNeedle hasn't matched it yet"
		],
		[
			'in a folder that keeps its tags',
			{
				edition_kind: 'none',
				release_mbid: null,
				state: 'unidentified',
				reason: { code: 'FOLDER_KEEPS_TAGS', message: '', action: '' }
			},
			'No edition yet',
			null,
			'This folder keeps its own tags'
		],
		[
			'unmatched',
			{ edition_kind: 'none', release_mbid: null, state: 'unmatched' },
			'No edition yet',
			null,
			'Nothing on MusicBrainz fits it yet'
		]
	])('reads right for an edition %s', async (_case, overrides, line1, chip, line2) => {
		h.status = status(overrides as Partial<EditionStatus>);
		await render(EditionPlate, { copies: [copy], groupMbid: 'group-1' });

		await expect.element(plate()).toHaveAccessibleName(new RegExp(`^Edition: ${line1}\\.`));
		await expect.element(plate().getByText(line1, { exact: true })).toBeVisible();
		await expect.element(plate().getByText(line2)).toBeVisible();
		if (chip) await expect.element(plate().getByText(chip, { exact: true })).toBeVisible();
	});

	it('is plain text with no way in for a user who cannot change editions', async () => {
		h.isTrusted = false;
		await render(EditionPlate, { copies: [copy], groupMbid: 'group-1' });

		await expect.element(page.getByRole('group', { name: /^Edition: CD edition/ })).toBeVisible();
		await expect.element(plate()).not.toBeInTheDocument();
	});

	it('opens the sheet on the copy it shows', async () => {
		await openSheet();

		await expect
			.element(sheet().getByRole('button', { name: /CD edition.*Yours now/ }))
			.toHaveAttribute('aria-pressed', 'true');
		await expect.element(sheet().getByText('All 4 of your files fit this edition.')).toBeVisible();
	});

	it('opens the sheet from ?edition=choose and takes the query off the address', async () => {
		h.url = new URL('http://localhost/album/group-1?edition=choose');
		await render(EditionPlate, { copies: [copy], groupMbid: 'group-1' });

		await expect.element(sheet()).toBeVisible();
		expect(String(h.goto.mock.calls[0][0])).toBe('http://localhost/album/group-1');
	});
});

describe('the edition sheet', () => {
	it('stays open on what happens next after "Use this edition", until Done', async () => {
		h.choose.mockResolvedValue({
			album_id: 'local-1',
			extra_track_ids: ['file-1'],
			missing_titles: ['Blue Hour'],
			release_group_mbid: 'group-1',
			release_mbid: 'release-remaster',
			retag_files: [],
			status: status({ release_mbid: 'release-remaster', state: 'chosen' })
		});
		await openSheet();
		await sheet()
			.getByRole('button', { name: /5th anniversary remaster/ })
			.click();

		await sheet().getByRole('button', { name: 'Use this edition' }).click();

		await expect
			.element(sheet().getByText('This is your edition now: 5th anniversary remaster'))
			.toBeVisible();
		expect(h.choose).toHaveBeenCalledWith(
			expect.objectContaining({ localId: 'local-1', releaseMbid: 'release-remaster' })
		);
		await expect.element(sheet()).toBeVisible();

		await sheet().getByRole('button', { name: 'Done' }).click();
		await expect.element(sheet()).not.toBeInTheDocument();
	});

	it('shows a file that is another version of a track as one row with both lengths', async () => {
		await openSheet();
		await sheet()
			.getByRole('button', { name: /5th anniversary remaster/ })
			.click();

		const blueHour = sheet().getByRole('row').filter({ hasText: 'Blue Hour' });
		await expect.element(blueHour).toBeVisible();
		expect(blueHour.elements()).toHaveLength(1);
		await expect.element(blueHour.getByText('Another version')).toBeVisible();
		await expect.element(blueHour.getByText('4:56')).toBeVisible();
		await expect.element(blueHour.getByText('4:11')).toBeVisible();
		await expect
			.element(
				sheet().getByText(
					'3 of your 4 files fit this edition. 1 of your files is another version of a track on it.'
				)
			)
			.toBeVisible();
	});

	it('on a phone lists full-width rows, then shows one edition with the action at the bottom', async () => {
		await page.viewport(390, 844);
		await openSheet();
		const remaster = sheet().getByRole('button', { name: /5th anniversary remaster/ });
		expect(remaster.element().getBoundingClientRect().width).toBeGreaterThanOrEqual(390 - 40);
		await expect
			.element(sheet().getByRole('region', { name: 'Selected edition', includeHidden: true }))
			.not.toBeVisible();

		await remaster.click();

		await expect.element(sheet().getByRole('button', { name: 'All editions' })).toBeVisible();
		await expect
			.element(sheet().getByRole('region', { name: 'Editions', exact: true, includeHidden: true }))
			.not.toBeVisible();
		const use = sheet().getByRole('button', { name: 'Use this edition' });
		await expect.element(use).toBeVisible();
		await expect.element(use).toBeInViewport();
		expect(use.element().getBoundingClientRect().bottom).toBeGreaterThan(844 - 80);
	});

	it('keeps the selection in view when the filter hides it', async () => {
		h.editions = [
			...h.editions,
			...['2001', '2005', '2010', '2012', '2016'].map((year) =>
				edition(`release-${year}`, `${year}-01-01`)
			)
		];
		await openSheet();

		await sheet().getByRole('searchbox', { name: 'Filter editions' }).fill('vinyl');

		await expect.element(sheet().getByText('Hidden by your filter')).toBeVisible();
		await expect.element(sheet().getByText('All 4 of your files fit this edition.')).toBeVisible();
	});

	it('says MusicBrainz is not answering, offers Retry and holds "Use this edition"', async () => {
		delete h.plans['release-remaster'];
		h.planErrors = { 'release-remaster': new ApiError(503, 'Service unavailable', 'UPSTREAM') };
		await openSheet();
		await sheet()
			.getByRole('button', { name: /5th anniversary remaster/ })
			.click();

		await expect
			.element(sheet().getByText(/MusicBrainz isn't answering, so this edition can't be compared/))
			.toBeVisible();
		await expect.element(sheet().getByRole('button', { name: 'Retry' })).toBeVisible();
		await expect.element(sheet().getByRole('button', { name: 'Use this edition' })).toBeDisabled();
	});
});
