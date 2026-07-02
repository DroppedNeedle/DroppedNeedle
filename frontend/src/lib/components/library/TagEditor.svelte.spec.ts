import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { LibraryTrack, TrackTagUpdate } from '$lib/types';

const { mockGet, mockSave } = vi.hoisted(() => ({ mockGet: vi.fn(), mockSave: vi.fn() }));

vi.mock('$lib/api/client', () => ({
	api: { global: { get: (...args: unknown[]) => mockGet(...args) } }
}));

vi.mock('$lib/queries/library/LibraryMutations.svelte', () => ({
	updateTrackTags: () => ({ mutateAsync: mockSave, isPending: false })
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

import TagEditor from './TagEditor.svelte';

const track: LibraryTrack = {
	id: 'file-1',
	recording_mbid: 'rec-1',
	disc_number: 1,
	track_number: 1,
	track_title: 'Airbag',
	artist_name: 'Radiohead',
	file_path: '/music/Radiohead/OK Computer/01 Airbag.flac',
	file_format: 'flac',
	bit_rate: 900,
	sample_rate: 44100,
	bit_depth: 16,
	duration_seconds: 260,
	file_size_bytes: 1048576,
	current_tier: 'lossless',
	below_cutoff: false
};

const loadedTags: TrackTagUpdate = {
	title: 'Airbag',
	artist: 'Radiohead',
	album: 'OK Computer',
	track_number: 1,
	album_artist: 'Radiohead',
	disc_number: 1,
	year: 1997,
	genre: 'Rock',
	musicbrainz_release_group_id: 'rg-ok',
	musicbrainz_release_id: null,
	musicbrainz_recording_id: null,
	musicbrainz_artist_id: null,
	musicbrainz_album_artist_id: null
};

function renderEditor() {
	return render(TagEditor, {
		props: { track, releaseGroupMbid: 'rg-ok', open: true }
	} as Parameters<typeof render<typeof TagEditor>>[1]);
}

describe('TagEditor.svelte', () => {
	beforeEach(() => {
		mockGet.mockReset();
		mockGet.mockResolvedValue({ ...loadedTags });
		mockSave.mockReset();
		mockSave.mockResolvedValue({});
	});

	it('fetches the track tags from disk when opened', async () => {
		renderEditor();
		await expect.element(page.getByText('Edit tags')).toBeVisible();
		// Save is disabled until the prefill load resolves; waiting for it to enable
		// guarantees the GET has run.
		await expect.element(page.getByRole('button', { name: 'Save' })).toBeEnabled();
		expect(mockGet).toHaveBeenCalledWith('/api/v1/library/tracks/file-1/tags');
	});

	it('saves the prefilled tags immediately when no MBID changed', async () => {
		renderEditor();
		// click() auto-waits for the button to become enabled (i.e. load finished).
		await page.getByRole('button', { name: 'Save' }).click();
		expect(mockSave).toHaveBeenCalledWith(
			expect.objectContaining({
				fileId: 'file-1',
				releaseGroupMbid: 'rg-ok',
				tags: expect.objectContaining({ title: 'Airbag', album: 'OK Computer' })
			})
		);
	});
});
