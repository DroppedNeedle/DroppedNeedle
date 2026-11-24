import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it } from 'vitest';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';
import type { HeldEntry, HeldFile } from '$lib/features/downloads/queries/types';

import Downloads from './+page.svelte';

function file(id: number): HeldFile {
	return {
		id,
		original_name: `Tidewater/${id}.flac`,
		release_mbid: null,
		release_track_mbid: null,
		recording_mbid: null,
		disc_number: 1,
		track_number: id,
		track_title: `Track ${id}`,
		file_format: 'flac',
		duration_seconds: 180,
		expected_duration_seconds: null,
		evidence_title: null,
		evidence_artist: null,
		evidence_score: null,
		reason: 'tag_mismatch',
		reason_text: 'Its tags name another album.',
		reason_action: 'Listen, then import or discard it.',
		reason_kind: 'hold',
		reason_detail: null,
		source: 'upload',
		source_task_id: null,
		created_at: 1,
		actions: ['import']
	};
}

const held: HeldEntry = {
	id: 1,
	origin: 'upload',
	state: 'held',
	release_group_mbid: null,
	recording_mbid: null,
	drop_item_id: 7,
	artist_name: 'Orla',
	album_title: 'Tidewater',
	created_at: 1,
	expires_at: null,
	files: [file(1), file(2)]
};

beforeEach(() => {
	localStorage.clear();
	authStore.setUser(
		toAuthUser({
			id: 'curator-1',
			display_name: 'Curator',
			role: 'trusted',
			email: null,
			avatar_url: null,
			username: 'curator',
			username_display: 'Curator'
		})
	);
});

describe('the Downloads page with no download source on', () => {
	it('still lists the files kept for a decision', async () => {
		fakeApi({
			'GET /api/v3/home/integration-status': {
				download_client: false,
				jellyfin: false,
				lastfm: false,
				library: true,
				listenbrainz: false,
				localfiles: false,
				navidrome: false,
				plex: false,
				youtube: false,
				youtube_api: false
			},
			'GET /api/v3/held': { entries: [held] }
		});
		// The source switch is read through the app's own query client.
		await renderWithQuery(Downloads, {}, { client: queryClient });

		await expect
			.element(page.getByRole('heading', { name: 'Download client not configured' }))
			.toBeVisible();
		const card = page.getByRole('article').filter({ hasText: 'Tidewater' });
		await expect.element(card.getByRole('button', { name: 'Import all' })).toBeVisible();
	});
});
