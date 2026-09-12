import { page } from '@vitest/browser/context';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	accessQuery: { data: { allowed: true } } as { data?: { allowed: boolean } },
	downloadAlbum: vi.fn()
}));

vi.mock('$lib/queries/local/LocalQueries.svelte', () => ({
	getDownloadAccessQuery: () => h.accessQuery
}));
vi.mock('$lib/utils/downloadActions', () => ({
	downloadAlbumArchive: (...args: unknown[]) => h.downloadAlbum(...args),
	downloadTrackFile: vi.fn()
}));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { isAdmin: false, isTrusted: false, user: { id: 'user-1' } },
	LAST_USER_ID_KEY: 'test:last-user'
}));
vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: {
		addToQueue: vi.fn(),
		playNext: vi.fn(),
		addMultipleToQueue: vi.fn(),
		playMultipleNext: vi.fn()
	}
}));

import { integrationStore } from '$lib/stores/integration';
import AlbumCardOverlay from './AlbumCardOverlay.svelte';

async function renderOverlay() {
	await render(AlbumCardOverlay, {
		mbid: 'mbid-1',
		albumName: 'Avalon',
		artistName: 'Roxy Music',
		coverUrl: null
	});
}

async function openMenu() {
	await expect.element(page.getByLabelText('More actions')).toBeVisible();
	await page.getByLabelText('More actions').click();
}

beforeEach(() => {
	vi.clearAllMocks();
	h.accessQuery = { data: { allowed: true } };
	integrationStore.setStatus({ localfiles: true });
});

afterEach(() => {
	integrationStore.reset();
});

describe('AlbumCardOverlay Download Album item', () => {
	it('downloads the album zip for an allowed viewer', async () => {
		expect.assertions(3);
		await renderOverlay();
		await openMenu();

		await expect.element(page.getByRole('menuitem', { name: 'Download Album' })).toBeVisible();
		await page.getByRole('menuitem', { name: 'Download Album' }).click();
		expect(h.downloadAlbum).toHaveBeenCalledWith('/api/v1/download/local/album/mbid/mbid-1');
	});

	it('omits the item when downloads are restricted', async () => {
		expect.assertions(3);
		h.accessQuery = { data: { allowed: false } };
		await renderOverlay();
		await openMenu();

		await expect.element(page.getByRole('menuitem', { name: 'Add to Queue' })).toBeVisible();
		expect(page.getByRole('menuitem', { name: 'Download Album' }).elements()).toHaveLength(0);
	});

	it('keeps the item while the permission is still loading (fail-open)', async () => {
		expect.assertions(2);
		h.accessQuery = {};
		await renderOverlay();
		await openMenu();

		await expect.element(page.getByRole('menuitem', { name: 'Download Album' })).toBeVisible();
	});
});
