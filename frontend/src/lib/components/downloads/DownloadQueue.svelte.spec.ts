import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it } from 'vitest';

import { toAuthUser } from '$lib/queries/auth/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import type { HeldEntry, HeldFile } from '$lib/types';

import DownloadQueue from './DownloadQueue.svelte';

const year = new Date().getFullYear();
const DOWNLOAD_EXPIRES = new Date(year, 5, 23).getTime();
const UPLOAD_EXPIRES = new Date(year, 6, 4).getTime();

function file(id: number, over: Partial<HeldFile> = {}): HeldFile {
	return {
		id,
		original_name: `Drift/${id}.flac`,
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
		source: 'soulseek',
		source_task_id: 'task-1',
		created_at: 1,
		actions: ['import'],
		...over
	};
}

const download: HeldEntry = {
	id: 1,
	origin: 'download',
	state: 'held',
	release_group_mbid: null,
	recording_mbid: null,
	drop_item_id: null,
	artist_name: 'Vela',
	album_title: 'Drift',
	created_at: 1,
	expires_at: DOWNLOAD_EXPIRES,
	files: [file(1), file(2)]
};

const upload: HeldEntry = {
	id: 2,
	origin: 'upload',
	state: 'held',
	release_group_mbid: null,
	recording_mbid: null,
	drop_item_id: 7,
	artist_name: 'Orla',
	album_title: 'Tidewater',
	created_at: 2,
	expires_at: UPLOAD_EXPIRES,
	files: [
		file(3, { source: 'upload', source_task_id: null }),
		file(4, { source: 'upload', source_task_id: null })
	]
};

const keptUntil = (ms: number) =>
	`Kept until ${new Date(ms).toLocaleDateString(undefined, { day: 'numeric', month: 'short' })}`;

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

describe('the held list on the Downloads queue', () => {
	it('shows a held download and a held upload as two entries with the same actions', async () => {
		fakeApi({
			'GET /api/v3/downloads/tasks': { items: [], page: 1, page_size: 100 },
			'GET /api/v3/held': { entries: [download, upload] }
		});
		await renderWithQuery(DownloadQueue, {});

		await expect.element(page.getByText('2 to decide', { exact: true })).toBeVisible();

		for (const [title, expires] of [
			['Drift', DOWNLOAD_EXPIRES],
			['Tidewater', UPLOAD_EXPIRES]
		] as const) {
			const card = page.getByRole('article').filter({ hasText: title });
			await expect.element(card.getByText(keptUntil(expires))).toBeVisible();
			await expect.element(card.getByRole('button', { name: 'Import all' })).toBeVisible();
			await expect.element(card.getByRole('button', { name: 'Match…' })).toBeVisible();
			await expect.element(card.getByRole('button', { name: 'Discard all' })).toBeVisible();
		}
	});

	it('shows when a file the library could not take is tried again, and while it is', async () => {
		const at = new Date();
		at.setHours(23, 59, 59, 0);
		const occupied = (id: number) =>
			file(id, {
				release_track_mbid: `track-${id}`,
				reason: 'target_occupied',
				reason_text: 'Another file already sits where this would go in your library.',
				reason_kind: 'library',
				actions: ['retry']
			});
		fakeApi({
			'GET /api/v3/downloads/tasks': { items: [], page: 1, page_size: 100 },
			'GET /api/v3/held': {
				entries: [
					{ ...download, files: [occupied(1), file(2)], next_retry_at: at.getTime() },
					{ ...upload, state: 'placing', files: [occupied(3)] }
				]
			}
		});
		await renderWithQuery(DownloadQueue, {});

		const time = at.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
		const waiting = page.getByRole('article').filter({ hasText: 'Drift' });
		await expect.element(waiting.getByText(`Tries again by itself at ${time}`)).toBeVisible();
		const placing = page.getByRole('article').filter({ hasText: 'Tidewater' });
		await expect.element(placing.getByText('Retrying now')).toBeVisible();
	});
});
