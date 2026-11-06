import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it } from 'vitest';

import { toAuthUser } from '$lib/features/auth/queries/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import type { HeldEntry, HeldFile } from '$lib/features/downloads/queries/types';

import HeldEntryCard from './HeldEntryCard.svelte';

function file(id: number): HeldFile {
	return {
		id,
		original_name: `Odds/0${id} Glory Box.flac`,
		release_mbid: null,
		release_track_mbid: null,
		recording_mbid: null,
		disc_number: 1,
		track_number: id,
		track_title: null,
		file_format: 'flac',
		duration_seconds: 180,
		expected_duration_seconds: null,
		evidence_title: null,
		evidence_artist: null,
		evidence_score: null,
		reason: 'no_album_match',
		reason_text: 'No album matched these files.',
		reason_action: 'Pick the album with Match, or discard them.',
		reason_kind: 'hold',
		reason_detail: null,
		source: 'import_folder',
		source_task_id: null,
		created_at: 1,
		actions: []
	};
}

const leftover: HeldEntry = {
	id: 8,
	origin: 'import_folder',
	state: 'held',
	release_group_mbid: null,
	recording_mbid: null,
	drop_item_id: 4,
	artist_name: null,
	album_title: null,
	created_at: 1,
	expires_at: null,
	files: [file(1), file(2)]
};

beforeEach(() => {
	localStorage.clear();
	authStore.setUser(
		toAuthUser({
			id: 'admin-1',
			display_name: 'Admin',
			role: 'admin',
			email: null,
			avatar_url: null,
			username: 'admin',
			username_display: 'Admin'
		})
	);
});

describe('a leftover of the import folder', () => {
	it('stays until someone decides, and Discard says it deletes from the import folder', async () => {
		const api = fakeApi({
			'POST /api/v3/held/{entry_id}/discard': {
				imported: 0,
				already_owned: 0,
				discarded: 2,
				still_held: 0,
				message: null
			}
		});
		await renderWithQuery(HeldEntryCard, { entry: leftover });

		await expect.element(page.getByText('2 files · Import folder', { exact: false })).toBeVisible();
		await expect.element(page.getByText('Stays until you decide')).toBeVisible();

		await page.getByRole('button', { name: 'Discard all' }).click();
		const dialog = page.getByRole('dialog');
		await expect.element(dialog.getByText('Discard 2 files?')).toBeVisible();
		await expect
			.element(
				dialog.getByText(
					'They are deleted from the import folder. Nothing in your library changes.'
				)
			)
			.toBeVisible();
		await dialog.getByRole('button', { name: 'Discard' }).click();
		await expect.poll(() => api.calls('POST /api/v3/held/{entry_id}/discard').length).toBe(1);
	});
});
