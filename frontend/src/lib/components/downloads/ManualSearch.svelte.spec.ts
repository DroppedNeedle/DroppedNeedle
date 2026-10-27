import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it } from 'vitest';

import { toAuthUser } from '$lib/queries/auth/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

import ManualSearch from './ManualSearch.svelte';

type Job = RouteData<'GET /api/v3/downloads/search/{job_id}'>;
type Candidate = Job['candidates'][number];

function candidate(index: number, over: Partial<Candidate> = {}): Candidate {
	return {
		candidate_index: index,
		source: 'soulseek',
		title: 'Dummy (1994)',
		tier: 'recommended',
		note: { code: 'complete', text: 'Holds all 11 tracks of this edition.' },
		refusal: [],
		username: 'collector',
		files: [],
		file_count: 11,
		size_bytes: 320_000_000,
		tracks_matched: 11,
		length_misses: 0,
		tracks_total: 11,
		format: 'FLAC 16-bit',
		has_free_slot: true,
		queue_length: 0,
		upload_speed: 1_000_000,
		indexer: null,
		posted_at: null,
		grabs: null,
		score: null,
		...over
	};
}

const job: Job = {
	job_id: 'job-1',
	status: 'completed',
	artist_name: 'Portishead',
	album_title: 'Dummy',
	year: 1994,
	release_group_mbid: 'rg-1',
	release_mbid: 'rel-1',
	tracks_total: 11,
	candidate_count: 2,
	candidates: [
		candidate(0),
		candidate(1, {
			title: 'Dummy [MP3]',
			username: 'mp3peer',
			tier: 'refused',
			refusal: ['quality_outside_range'],
			format: 'MP3 320'
		})
	],
	reason: null,
	task_id: null
};

beforeEach(() => {
	localStorage.clear();
	authStore.setUser(
		toAuthUser({
			id: 'user-1',
			display_name: 'User',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'user',
			username_display: 'User'
		})
	);
});

describe('picking a source by hand', () => {
	it('asks before picking a candidate outside the download settings', async () => {
		const api = fakeApi({
			'GET /api/v3/downloads/search/{job_id}': job,
			'GET /api/v3/plugins/sources': { sources: [] },
			'POST /api/v3/downloads/search/{job_id}/pick': { task_id: 'task-1' }
		});
		await renderWithQuery(ManualSearch, { jobId: 'job-1' });

		const refused = page.getByRole('article').filter({ hasText: 'mp3peer' });
		await expect.element(refused.getByText('Quality outside your range')).toBeVisible();

		await refused.getByRole('button', { name: /^Pick Dummy \[MP3\]/ }).click();
		const dialog = page.getByRole('dialog');
		await expect
			.element(
				dialog.getByText(
					'Outside your download settings: quality outside your range. Download it anyway?'
				)
			)
			.toBeVisible();
		await dialog.getByRole('button', { name: 'Cancel' }).click();
		await expect.element(page.getByRole('dialog')).not.toBeInTheDocument();
		expect(api.calls('POST /api/v3/downloads/search/{job_id}/pick')).toHaveLength(0);

		await refused.getByRole('button', { name: /^Pick Dummy \[MP3\]/ }).click();
		await page.getByRole('dialog').getByRole('button', { name: 'Download anyway' }).click();
		await expect
			.poll(() => api.calls('POST /api/v3/downloads/search/{job_id}/pick').map((c) => c.body))
			.toEqual([{ candidate_index: 1, confirm_policy_break: true }]);
	});
});
