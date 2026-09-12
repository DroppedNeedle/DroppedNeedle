import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { LocalAlbumSummary, LocalAlbumMatch } from '$lib/types';

const h = vi.hoisted(() => ({
	apiGet: vi.fn(),
	downloadAlbum: vi.fn(),
	downloadTrack: vi.fn(),
	goto: vi.fn()
}));

vi.mock('$app/navigation', () => ({ goto: (...args: unknown[]) => h.goto(...args) }));
vi.mock('$lib/api/client', async (importOriginal) => {
	const mod = await importOriginal<typeof import('$lib/api/client')>();
	return {
		...mod,
		api: {
			...mod.api,
			global: { ...mod.api.global, get: (...args: unknown[]) => h.apiGet(...args) }
		}
	};
});
vi.mock('$lib/utils/downloadActions', () => ({
	downloadAlbumArchive: (...args: unknown[]) => h.downloadAlbum(...args),
	downloadTrackFile: (...args: unknown[]) => h.downloadTrack(...args)
}));
vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: {
		nowPlaying: null,
		currentQueueItem: null,
		isPlaying: false,
		playQueue: vi.fn(),
		addToQueue: vi.fn(),
		playNext: vi.fn(),
		addMultipleToQueue: vi.fn(),
		playMultipleNext: vi.fn()
	}
}));
vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

import SourceAlbumModal from './SourceAlbumModal.svelte';

function albumFixture(downloadAllowed?: boolean): LocalAlbumSummary {
	return {
		musicbrainz_id: 'mbid-1',
		name: 'Avalon',
		artist_name: 'Roxy Music',
		track_count: 1,
		total_size_bytes: 1024,
		primary_format: 'FLAC',
		cover_url: null,
		...(downloadAllowed === undefined ? {} : { download_allowed: downloadAllowed })
	};
}

const matchFixture: LocalAlbumMatch = {
	found: true,
	musicbrainz_id: 'mbid-1',
	tracks: [
		{
			track_file_id: 'file-1',
			title: 'Avalon Song',
			track_number: 1,
			disc_number: 1,
			size_bytes: 1024,
			format: 'FLAC'
		}
	],
	total_size_bytes: 1024,
	primary_format: 'FLAC'
};

async function renderModal(album: LocalAlbumSummary) {
	await render(SourceAlbumModal, {
		props: { open: true, sourceType: 'local', album, onclose: () => {} }
	} as unknown as Parameters<typeof render>[1]);
	// The dialog relies on app CSS (absent in the test env) for visibility, so
	// set the open attribute: visible like in prod, but without showModal's
	// top-layer backdrop (which would sit over the portaled dropdown menus and
	// intercept their clicks).
	const dialog = document.querySelector('dialog');
	if (!(dialog instanceof HTMLDialogElement)) throw new Error('source modal did not render');
	dialog.open = true;
}

async function openMenu(index: number) {
	const triggers = page.getByLabelText('More actions');
	await expect.element(triggers.first()).toBeVisible();
	await (await triggers.all())[index].click();
}

beforeEach(() => {
	vi.clearAllMocks();
	h.apiGet.mockResolvedValue(matchFixture);
});

describe('SourceAlbumModal local downloads', () => {
	it('downloads the album zip from the bulk menu', async () => {
		expect.assertions(4);
		await renderModal(albumFixture(true));

		await expect.element(page.getByText('Play All')).toBeVisible();
		await openMenu(0);
		await expect.element(page.getByRole('menuitem', { name: 'Download Album' })).toBeVisible();
		await page.getByRole('menuitem', { name: 'Download Album' }).click();
		expect(h.downloadAlbum).toHaveBeenCalledWith('/api/v1/download/local/album/mbid/mbid-1');
	});

	it('downloads the file from the track menu', async () => {
		expect.assertions(4);
		await renderModal(albumFixture(true));

		await expect.element(page.getByText('Avalon Song')).toBeVisible();
		await openMenu(1);
		await expect.element(page.getByRole('menuitem', { name: 'Download' })).toBeVisible();
		await page.getByRole('menuitem', { name: 'Download' }).click();
		expect(h.downloadTrack).toHaveBeenCalledWith(
			'/api/v1/download/local/track/file-1',
			'Avalon Song'
		);
	});

	it('omits both Download items when downloads are restricted', async () => {
		expect.assertions(7);
		await renderModal(albumFixture(false));

		// Row menu first: an open dropdown overlaps the trigger below it, so a
		// second menu can only be opened after the first is closed.
		await expect.element(page.getByText('Avalon Song')).toBeVisible();
		await openMenu(1);
		await expect.element(page.getByRole('menuitem', { name: 'Add to Queue' })).toBeVisible();
		expect(page.getByRole('menuitem', { name: 'Download' }).elements()).toHaveLength(0);

		await openMenu(0);
		await expect.element(page.getByRole('menuitem', { name: 'Add All to Queue' })).toBeVisible();
		expect(page.getByRole('menuitem', { name: 'Download Album' }).elements()).toHaveLength(0);
	});
});
