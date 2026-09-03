import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { HeldImport } from '$lib/types';

const h = vi.hoisted(() => ({
	importMut: vi.fn(),
	discardMut: vi.fn(),
	reverifyMut: vi.fn(),
	importError: null as { message: string } | null,
	discardError: null as { message: string } | null,
	reverifyError: null as { message: string } | null
}));

vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
	importHeldTrack: () => ({
		mutate: h.importMut,
		isPending: false,
		get error() {
			return h.importError;
		}
	}),
	discardHeldTrack: () => ({
		mutate: h.discardMut,
		isPending: false,
		get error() {
			return h.discardError;
		}
	}),
	reverifyHeldTrack: () => ({
		mutate: h.reverifyMut,
		isPending: false,
		get error() {
			return h.reverifyError;
		}
	})
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
		release_mbid: null,
		release_track_mbid: null,
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
		expected_duration_seconds: 390,
		reason: 'fingerprint_mismatch',
		reason_detail: null,
		source: 'usenet',
		source_task_id: 't-1',
		created_at: 0,
		evidence_title: "Nobody's Fault but Mine",
		evidence_artist: 'Led Zeppelin',
		evidence_score: 0.99,
		management_retry_count: 0,
		management_next_retry_at: null,
		...overrides
	};
}

describe('HeldTrackCard', () => {
	beforeEach(() => {
		h.importMut.mockReset();
		h.discardMut.mockReset();
		h.reverifyMut.mockReset();
		h.importError = null;
		h.discardError = null;
		h.reverifyError = null;
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

	it('renders a server error inline and leaves the buttons enabled for retry', async () => {
		h.importError = {
			message: 'No library root is configured - restore one in Settings → Library, then try again.'
		};
		renderCard(held());

		const alert = page.getByRole('alert');
		await expect.element(alert).toHaveTextContent(/No library root is configured/);
		const importButton = page.getByRole('button', { name: /Import anyway/ });
		const discardButton = page.getByRole('button', { name: /Discard/ });
		await expect.element(importButton).toBeEnabled();
		await expect.element(discardButton).toBeEnabled();

		await importButton.click();
		expect(h.importMut).toHaveBeenCalledTimes(1);
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

	it('shows the file length against the expected length', async () => {
		renderCard(held());
		await expect.element(page.getByText(/File length 6:28/)).toBeVisible();
		await expect.element(page.getByText(/expected 6:30/)).toBeVisible();
	});

	it('hides the length comparison when the expected length is unknown', async () => {
		renderCard(held({ expected_duration_seconds: null }));
		await expect.element(page.getByText(/File length/)).not.toBeInTheDocument();
	});

	it('hides the length comparison when the file duration is unknown', async () => {
		renderCard(held({ duration_seconds: null, expected_duration_seconds: 390 }));
		await expect.element(page.getByText(/File length/)).not.toBeInTheDocument();
	});

	it('re-checks the held track on "Re-check"', async () => {
		renderCard(held());
		await page.getByRole('button', { name: 'Re-check' }).click();
		expect(h.reverifyMut).toHaveBeenCalledWith(
			{ id: 7, release_group_mbid: 'rg-1' },
			expect.objectContaining({ onSuccess: expect.any(Function) })
		);
		expect(h.importMut).not.toHaveBeenCalled();
		expect(h.discardMut).not.toHaveBeenCalled();
	});
});
