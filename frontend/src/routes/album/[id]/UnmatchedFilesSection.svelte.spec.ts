import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { LibraryTrack } from '$lib/types';

const h = vi.hoisted(() => ({ removeMut: vi.fn() }));

vi.mock('$lib/queries/library/LibraryMutations.svelte', () => ({
	removeLibraryTrack: () => ({ mutate: h.removeMut, isPending: false })
}));

import UnmatchedFilesSection from './UnmatchedFilesSection.svelte';

type RenderOpts = Parameters<typeof render<typeof UnmatchedFilesSection>>[1];
function renderSection(orphans: LibraryTrack[], canRemove = true) {
	return render(UnmatchedFilesSection, {
		props: { orphans, albumMbid: 'rg-1', canRemove }
	} as unknown as RenderOpts);
}

function orphan(overrides: Partial<LibraryTrack> = {}): LibraryTrack {
	return {
		id: 'f-wrong',
		recording_mbid: null,
		disc_number: 1,
		track_number: 2,
		track_title: 'Arrival in Ashford',
		artist_name: 'Dan Romer',
		file_path: '/music/Yan Qing/the arrival (2026)/0102 Arrival in Ashford.flac',
		file_format: 'flac',
		bit_rate: null,
		sample_rate: 48000,
		bit_depth: 24,
		duration_seconds: 137.24,
		file_size_bytes: 26984685,
		current_tier: null,
		below_cutoff: false,
		...overrides
	};
}

describe('UnmatchedFilesSection', () => {
	beforeEach(() => {
		h.removeMut.mockReset();
	});

	it('shows the file’s OWN tagged identity and the doesn’t-match state', async () => {
		renderSection([orphan()]);
		await expect.element(page.getByText(/Unmatched files/)).toBeVisible();
		await expect
			.element(page.getByText(/Stored under this album, but they don't match/))
			.toBeVisible();
		// the honesty payload: what the tags actually say
		await expect.element(page.getByText(/Dan Romer - Arrival in Ashford/)).toBeVisible();
		await expect.element(page.getByText(/Doesn't match/)).toBeVisible();
		await expect.element(page.getByText(/0102 Arrival in Ashford\.flac/)).toBeVisible();
	});

	it('renders nothing at all when every file matches', async () => {
		const { container } = renderSection([]);
		expect(container.querySelector('section')).toBeNull();
	});

	it('removal is a two-step confirm before the mutation fires', async () => {
		renderSection([orphan()]);
		await page.getByRole('button', { name: /^Remove$/ }).click();
		expect(h.removeMut).not.toHaveBeenCalled(); // armed, not fired
		await page.getByRole('button', { name: /Remove file/ }).click();
		expect(h.removeMut).toHaveBeenCalledWith(
			{ fileId: 'f-wrong', albumMbid: 'rg-1' },
			expect.objectContaining({ onSuccess: expect.any(Function) })
		);
	});

	it('backing out of the confirm keeps the file', async () => {
		renderSection([orphan()]);
		await page.getByRole('button', { name: /^Remove$/ }).click();
		await page.getByRole('button', { name: /Keep this file/ }).click();
		await expect.element(page.getByRole('button', { name: /^Remove$/ })).toBeVisible();
		expect(h.removeMut).not.toHaveBeenCalled();
	});

	it('hides removal from regular users, view stays honest for everyone', async () => {
		renderSection([orphan()], false);
		await expect.element(page.getByText(/Dan Romer - Arrival in Ashford/)).toBeVisible();
		expect(page.getByRole('button', { name: /^Remove$/ }).elements().length).toBe(0);
	});

	it('offers an inline preview without touching the play queue', async () => {
		renderSection([orphan()]);
		await expect.element(page.getByRole('button', { name: /Play a preview/ })).toBeVisible();
	});
});
