import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { UnmatchedFile } from './albumEdition';

const h = vi.hoisted(() => ({ removeMut: vi.fn() }));

vi.mock('$lib/queries/library/LibraryMutations.svelte', () => ({
	removeLibraryTrack: () => ({ mutate: h.removeMut, isPending: false })
}));

import UnmatchedFilesSection from './UnmatchedFilesSection.svelte';

type RenderOpts = Parameters<typeof render<typeof UnmatchedFilesSection>>[1];
async function renderSection(files: UnmatchedFile[], canRemove = true) {
	return await render(UnmatchedFilesSection, {
		props: { files, albumMbid: 'rg-1', albumId: 'local-1', canRemove }
	} as unknown as RenderOpts);
}

function orphan(overrides: Partial<UnmatchedFile> = {}): UnmatchedFile {
	return {
		id: 'f-wrong',
		title: 'Arrival in Ashford',
		artistName: 'Dan Romer',
		format: 'flac',
		lengthMs: 137240,
		label: 'Another version of track 2',
		note: null,
		...overrides
	};
}

describe('UnmatchedFilesSection', () => {
	beforeEach(() => {
		h.removeMut.mockReset();
	});

	it('removal is a two-step confirm before the mutation fires', async () => {
		await renderSection([orphan()]);
		await page.getByRole('button', { name: /^Remove$/ }).click();
		expect(h.removeMut).not.toHaveBeenCalled(); // armed, not fired
		await page.getByRole('button', { name: /Remove file/ }).click();
		expect(h.removeMut).toHaveBeenCalledTimes(1);
	});

	it('backing out of the confirm keeps the file', async () => {
		await renderSection([orphan()]);
		await page.getByRole('button', { name: /^Remove$/ }).click();
		await page.getByRole('button', { name: /Keep this file/ }).click();
		await expect.element(page.getByRole('button', { name: /^Remove$/ })).toBeVisible();
		expect(h.removeMut).not.toHaveBeenCalled();
	});

	it('hides removal from regular users, view stays honest for everyone', async () => {
		await renderSection([orphan()], false);
		await expect.element(page.getByText(/Dan Romer - Arrival in Ashford/)).toBeVisible();
		await expect.element(page.getByText('Another version of track 2')).toBeVisible();
		expect(page.getByRole('button', { name: /^Remove$/ }).elements().length).toBe(0);
	});
});
