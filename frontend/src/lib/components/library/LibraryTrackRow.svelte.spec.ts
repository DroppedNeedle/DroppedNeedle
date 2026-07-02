import { page } from '@vitest/browser/context';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import LibraryTrackRow from './LibraryTrackRow.svelte';
import type { LibraryFileMeta } from '$lib/types';

const meta: LibraryFileMeta = {
	id: 'file-1',
	recording_mbid: 'rec-airbag-0001',
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

function renderComponent() {
	return render(LibraryTrackRow, {
		props: { meta, releaseGroupMbid: 'rg-ok' }
	} as Parameters<typeof render<typeof LibraryTrackRow>>[1]);
}

describe('LibraryTrackRow.svelte', () => {
	it('shows the file path', async () => {
		renderComponent();
		await expect.element(page.getByText(meta.file_path)).toBeInTheDocument();
	});

	it('shows the recording MBID', async () => {
		renderComponent();
		await expect.element(page.getByText('rec-airbag-0001')).toBeInTheDocument();
	});

	it('does not show the admin Edit tags button for non-admins', async () => {
		renderComponent();
		await expect.element(page.getByText('Edit tags')).not.toBeInTheDocument();
	});
});
