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
	acquire: vi.fn(),
	seal: vi.fn(),
	leave: vi.fn(),
	downloads: [] as unknown[],
	latest: null as Record<string, unknown> | null,
	cancel: vi.fn(),
	isTrusted: true,
	isAdmin: false,
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
		},
		get isAdmin() {
			return h.isAdmin;
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
	getEditionTracksQuery: () => ({ data: undefined, isPending: true, refetch: vi.fn() }),
	chooseAlbumEdition: () => mutation((...args: unknown[]) => h.choose(...args)),
	confirmAlbumEdition: () => mutation((...args: unknown[]) => h.confirm(...args)),
	handBackAlbumEdition: () => mutation(),
	undoAlbumEdition: () => mutation(),
	acquireEdition: () => mutation((...args: unknown[]) => h.acquire(...args)),
	getEditionFetchQuery: () => ({
		get data() {
			return { latest: h.latest };
		}
	}),
	editionFetchLive: (edition: { state?: string } | null) =>
		edition?.state === 'waiting' || edition?.state === 'fetching',
	previewEditionTags: () => mutation(),
	getTagWriteQuery: () => ({ data: undefined, refetch: vi.fn() }),
	tagWriteFinished: vi.fn(),
	writeEditionTags: () => mutation(),
	undoTagWrite: () => mutation(),
	removeEditionExtras: () => mutation(),
	matchEditionFile: () => mutation(),
	unmatchEditionFile: () => mutation(),
	sealCustomEdition: () => mutation((...args: unknown[]) => h.seal(...args)),
	leaveAlbumUnmanaged: () => mutation((...args: unknown[]) => h.leave(...args))
}));
vi.mock('$lib/stores/integration', async () => {
	const { writable } = await import('svelte/store');
	return { integrationStore: writable({ download_client: true }) };
});
vi.mock('$lib/queries/downloads/DownloadQueries.svelte', () => ({
	getAlbumDownloadsQuery: () => ({
		get data() {
			return { items: h.downloads };
		}
	})
}));
vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
	cancelDownload: () => ({ mutate: h.cancel, isPending: false }),
	retryDownload: () => ({ mutate: vi.fn(), isPending: false }),
	stopAutoRetry: () => ({ mutate: vi.fn(), isPending: false }),
	tryNextSource: () => ({ mutate: vi.fn(), isPending: false })
}));
vi.mock('$lib/queries/downloads/DownloadSSE.svelte', () => ({
	createDownloadStream: () => ({
		start: vi.fn(),
		stop: vi.fn(),
		state: { progress: null, status: null, source: null, done: false }
	})
}));
vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibraryAlbumDetailQuery: () => ({ data: undefined })
}));
vi.mock('$lib/queries/library/RemovalQueries.svelte', () => ({
	getAlbumRemovalsQuery: () => ({ data: [] }),
	putBackRemoval: () => mutation(),
	invalidateAlbumRemovals: () => Promise.resolve()
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
import { integrationStore } from '$lib/stores/integration';
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
		keeps_own_tags: false,
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

// The remaster with one more track no file holds: 1 missing, 1 in another version.
function gapPlan(): EditionPlan {
	const plan = remasterPlan();
	return {
		...plan,
		missing_count: 2,
		tracks: [
			...plan.tracks,
			{
				disc_number: 1,
				position: 5,
				length_ms: minutes(3, 2),
				local_track_id: null,
				recording_mbid: 'rec-bonus',
				release_track_mbid: 're-bonus',
				title: 'Harbour Lights'
			}
		]
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
	h.acquire.mockReset();
	h.seal.mockReset();
	h.leave.mockReset();
	h.downloads = [];
	h.latest = null;
	h.cancel.mockReset();
	(integrationStore as unknown as { set: (value: unknown) => void }).set({ download_client: true });
	h.isTrusted = true;
	h.isAdmin = false;
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

describe('making your copy the chosen edition', () => {
	const chosenRemaster = {
		album_id: 'local-1',
		extra_track_ids: [],
		missing_titles: ['Harbour Lights'],
		release_group_mbid: 'group-1',
		release_mbid: 'release-remaster',
		retag_files: [],
		status: status({ release_mbid: 'release-remaster', state: 'chosen' })
	};

	async function pickRemaster(): Promise<void> {
		h.plans['release-remaster'] = gapPlan();
		h.choose.mockResolvedValue(chosenRemaster);
		await openSheet();
		await sheet()
			.getByRole('button', { name: /5th anniversary remaster/ })
			.click();
	}

	it('gets the missing tracks and replaces your versions by itself, with each track live in Next steps', async () => {
		h.acquire.mockResolvedValue({
			status: 'started',
			message: "Downloading the 1 missing track of this edition and 1 in this edition's version.",
			task_id: 'task-1'
		});
		h.latest = {
			task_id: 'task-1',
			release_mbid: 'release-remaster',
			state: 'fetching',
			tracks: [
				{ disc: 1, position: 2, state: 'fetching' },
				{ disc: 1, position: 5, state: 'waiting' }
			],
			replace_versions: true,
			can_retry: false,
			can_stop: true
		};
		await pickRemaster();
		await expect
			.element(sheet().getByRole('checkbox', { name: 'Get the 1 missing track' }))
			.toBeChecked();
		await expect
			.element(
				sheet().getByRole('checkbox', { name: 'Replace 1 track you have in another version' })
			)
			.toBeChecked();

		await sheet().getByRole('button', { name: 'Use this edition' }).click();

		await expect.element(sheet().getByText('Downloading 2 tracks.')).toBeVisible();
		const fetching = sheet().getByRole('list', { name: 'Tracks being fetched' });
		await expect.element(fetching.getByRole('listitem').nth(0)).toHaveTextContent(/Fetching/);
		await expect.element(fetching.getByRole('listitem').nth(1)).toHaveTextContent(/Waiting/);
		expect(h.acquire).toHaveBeenCalledWith({
			mbid: 'group-1',
			copyId: 'local-1',
			replaceVersions: true
		});
		await sheet().getByRole('button', { name: 'Stop' }).click();
		expect(h.cancel).toHaveBeenCalledWith('task-1');
	});

	it("shows a failed fetch's tracks when the sheet opens, and Retry asks the same way again", async () => {
		h.status = status({ release_mbid: 'release-remaster', state: 'chosen' });
		h.plans.current = gapPlan();
		h.latest = {
			task_id: 'task-1',
			release_mbid: 'release-remaster',
			state: 'failed',
			tracks: [
				{ disc: 1, position: 2, state: 'landed' },
				{ disc: 1, position: 5, state: 'failed' }
			],
			replace_versions: false,
			can_retry: true,
			can_stop: false
		};
		h.acquire.mockResolvedValue({ status: 'started', message: 'Downloading.', task_id: 'task-2' });
		await openSheet();

		await expect.element(sheet().getByText("1 track couldn't be downloaded.")).toBeVisible();
		const fetched = sheet().getByRole('list', { name: 'Tracks being fetched' });
		await expect
			.element(fetched.getByRole('listitem').nth(0))
			.toHaveTextContent(/Blue Hour.*Added/);
		await expect
			.element(fetched.getByRole('listitem').nth(1))
			.toHaveTextContent(/Harbour Lights.*Failed/);
		await expect.element(sheet().getByRole('button', { name: 'Stop' })).not.toBeInTheDocument();
		await sheet().getByRole('button', { name: 'Retry' }).click();
		expect(h.acquire).toHaveBeenCalledWith({
			mbid: 'group-1',
			copyId: 'local-1',
			replaceVersions: false
		});
	});

	it('leaves your versions alone with the second toggle off', async () => {
		h.acquire.mockResolvedValue({ status: 'started', message: 'Downloading.', task_id: null });
		await pickRemaster();
		await sheet()
			.getByRole('checkbox', { name: 'Replace 1 track you have in another version' })
			.click();

		await sheet().getByRole('button', { name: 'Use this edition' }).click();

		await expect.element(sheet().getByText('Downloading.')).toBeVisible();
		expect(h.acquire).toHaveBeenCalledWith(expect.objectContaining({ replaceVersions: false }));
	});

	it('with no download source still asks, and says the download waits for one', async () => {
		(integrationStore as unknown as { set: (value: unknown) => void }).set({
			download_client: false
		});
		h.acquire.mockResolvedValue({
			status: 'started',
			message:
				'No download source is set up, so this edition waits. It starts by itself once Soulseek, Usenet or a plugin source is saved.',
			task_id: null
		});
		await pickRemaster();
		await expect
			.element(sheet().getByRole('checkbox', { name: 'Get the 1 missing track' }))
			.toBeChecked();

		await sheet().getByRole('button', { name: 'Use this edition' }).click();

		await expect.element(sheet().getByText(/so this edition waits/)).toBeVisible();
		await expect.element(sheet().getByRole('link', { name: 'Download settings' })).toBeVisible();
		expect(h.acquire).toHaveBeenCalledWith(expect.objectContaining({ replaceVersions: true }));
	});

	it("shows a refused download's reason with Try again, and the choice stands", async () => {
		h.acquire.mockRejectedValueOnce(
			new ApiError(400, 'There is not enough free disk space for these downloads.', 'BAD_REQUEST')
		);
		await pickRemaster();

		await sheet().getByRole('button', { name: 'Use this edition' }).click();

		await expect
			.element(
				sheet()
					.getByRole('alert')
					.getByText(/not enough free disk space/)
			)
			.toBeVisible();
		await expect
			.element(sheet().getByText('This is your edition now: 5th anniversary remaster'))
			.toBeVisible();
		h.acquire.mockResolvedValueOnce({ status: 'started', message: 'Downloading.', task_id: null });
		await sheet().getByRole('button', { name: 'Try again' }).click();
		await expect.element(sheet().getByText('Downloading.')).toBeVisible();
		expect(h.acquire).toHaveBeenCalledTimes(2);
	});

	it('offers buttons, not toggles, on the current edition with gaps', async () => {
		h.status = status({ release_mbid: 'release-remaster', state: 'chosen' });
		h.plans.current = gapPlan();
		await openSheet();

		await expect
			.element(sheet().getByRole('button', { name: 'Get the 1 missing track' }))
			.toBeVisible();
		await expect
			.element(sheet().getByRole('button', { name: 'Replace 1 track you have in another version' }))
			.toBeVisible();
		await expect.element(sheet().getByRole('checkbox')).not.toBeInTheDocument();
	});
});

describe('when none of the editions is right', () => {
	const noneRight = () => sheet().getByRole('region', { name: 'None of these is right' });

	it('keeps the files as they are under the selected edition, for an admin', async () => {
		h.isAdmin = true;
		h.status = status({ undo_available: true });
		h.seal.mockResolvedValue({});
		await openSheet();
		await sheet()
			.getByRole('button', { name: /5th anniversary remaster/ })
			.click();

		await expect
			.element(
				noneRight().getByText(
					'DroppedNeedle manages your files as they are now, under Night Shift.'
				)
			)
			.toBeVisible();
		await noneRight().getByRole('button', { name: 'Keep your own track list' }).click();

		expect(h.seal).toHaveBeenCalledWith(
			expect.objectContaining({
				localId: 'local-1',
				releaseMbid: 'release-remaster',
				identityRevision: 2
			})
		);
		await expect
			.element(noneRight().getByText('Your own track list is the edition now.'))
			.toBeVisible();
		await expect.element(sheet().getByRole('button', { name: 'Undo last change' })).toBeVisible();
	});

	it('leaves the album unmanaged, showing a refusal in place', async () => {
		h.isAdmin = true;
		h.leave.mockRejectedValue(
			new ApiError(409, 'The album changed since this was shown.', 'STALE_REVISION', {
				action: 'Look again.'
			})
		);
		await openSheet();

		await noneRight().getByRole('button', { name: 'Leave this album unmanaged' }).click();

		expect(h.leave).toHaveBeenCalledWith(
			expect.objectContaining({ localId: 'local-1', identityRevision: 2 })
		);
		await expect
			.element(noneRight().getByRole('alert'))
			.toHaveTextContent('The album changed since this was shown. Look again.');
	});

	it('is not offered to a trusted user', async () => {
		await openSheet();

		await expect.element(sheet().getByText('All 4 of your files fit this edition.')).toBeVisible();
		await expect.element(noneRight()).not.toBeInTheDocument();
	});
});
