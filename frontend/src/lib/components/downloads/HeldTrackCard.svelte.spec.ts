import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { HeldImport } from '$lib/types';

const h = vi.hoisted(() => ({ importMut: vi.fn(), discardMut: vi.fn() }));

vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
	importHeldTrack: () => ({ mutate: h.importMut, isPending: false }),
	discardHeldTrack: () => ({ mutate: h.discardMut, isPending: false })
}));

import HeldTrackCard from './HeldTrackCard.svelte';

type RenderOpts = Parameters<typeof render<typeof HeldTrackCard>>[1];
function renderCard(item: HeldImport) {
	return render(HeldTrackCard, { props: { held: item } } as unknown as RenderOpts);
}

function held(overrides: Partial<HeldImport> = {}): HeldImport {
	return {
		id: 7,
		release_group_mbid: 'rg-1',
		recording_mbid: 'rec-3',
		track_number: 3,
		disc_number: 1,
		track_title: 'You Shook Me',
		artist_name: 'Led Zeppelin',
		album_title: 'Led Zeppelin',
		year: 1969,
		original_filename: '103.flac',
		file_format: 'flac',
		duration_seconds: 388,
		reason: 'fingerprint_mismatch',
		source: 'usenet',
		source_task_id: 't-1',
		created_at: 0,
		evidence_title: "Nobody's Fault but Mine",
		evidence_artist: 'Led Zeppelin',
		evidence_score: 0.99,
		...overrides
	};
}

describe('HeldTrackCard', () => {
	beforeEach(() => {
		h.importMut.mockReset();
		h.discardMut.mockReset();
	});

	it('shows the track, the couldn’t-verify state, and the AcoustID evidence', async () => {
		renderCard(held());
		await expect.element(page.getByText(/You Shook Me/)).toBeVisible();
		await expect.element(page.getByText(/Couldn't verify/i)).toBeVisible();
		await expect.element(page.getByText(/Nobody's Fault but Mine/)).toBeVisible();
		await expect.element(page.getByText(/99%/)).toBeVisible();
	});

	it('imports the held track on "Import anyway"', async () => {
		renderCard(held());
		await page.getByRole('button', { name: /Import anyway/ }).click();
		expect(h.importMut).toHaveBeenCalledWith(
			{ id: 7, release_group_mbid: 'rg-1' },
			expect.objectContaining({ onSuccess: expect.any(Function) })
		);
		expect(h.discardMut).not.toHaveBeenCalled();
	});

	it('discards the held track on "Discard"', async () => {
		renderCard(held());
		await page.getByRole('button', { name: /Discard/ }).click();
		expect(h.discardMut).toHaveBeenCalledWith(
			{ id: 7, release_group_mbid: 'rg-1' },
			expect.objectContaining({ onSuccess: expect.any(Function) })
		);
		expect(h.importMut).not.toHaveBeenCalled();
	});

	it('offers an audio preview: play control, scrubber, and the duration', async () => {
		renderCard(held());
		await expect.element(page.getByRole('button', { name: /Play a preview/ })).toBeVisible();
		await expect.element(page.getByRole('slider', { name: /Scrub preview/ })).toBeVisible();
		await expect.element(page.getByText('0:00 / 6:28')).toBeVisible(); // 388s duration
	});

	// evidence copy is reason-aware: tags for tag_mismatch, length for wrong_track
	// (the AcoustID wording above stays reserved for fingerprint_mismatch)

	it('describes tag_mismatch evidence as the file’s own tags, not AcoustID', async () => {
		renderCard(
			held({
				reason: 'tag_mismatch',
				evidence_title: 'Arrival in Ashford',
				evidence_artist: 'Dan Romer',
				evidence_score: null
			})
		);
		await expect.element(page.getByText(/file's own tags/i)).toBeVisible();
		await expect.element(page.getByText(/Arrival in Ashford/)).toBeVisible();
		await expect.element(page.getByText(/Dan Romer/)).toBeVisible();
	});

	it('describes wrong_track evidence as a length mismatch, closest match kept', async () => {
		renderCard(
			held({
				reason: 'wrong_track',
				evidence_title: 'the arrival',
				evidence_artist: null,
				evidence_score: null
			})
		);
		await expect.element(page.getByText(/wrong length/i)).toBeVisible();
		await expect.element(page.getByText(/closest copy/i)).toBeVisible();
	});
});
