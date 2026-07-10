import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { DropImportJob } from '$lib/queries/import/types';

const h = vi.hoisted(() => ({
	jobs: [] as DropImportJob[],
	isLoading: false,
	discard: vi.fn()
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: {
		get isAdmin() {
			return false;
		},
		get isTrusted() {
			return true;
		},
		get user() {
			return { id: 'user-1' };
		}
	}
}));

vi.mock('$lib/queries/import/DropImportQueries.svelte', () => ({
	getDropImportJobsQuery: () => ({
		get data() {
			return { jobs: h.jobs };
		},
		get isLoading() {
			return h.isLoading;
		}
	})
}));

vi.mock('$lib/queries/import/DropImportMutations.svelte', () => ({
	matchDropItemMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	discardDropItemMutation: () => ({ mutate: h.discard, isPending: false })
}));

vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getAlbumSearchQuery: () => ({ data: [], isFetching: false })
}));

import DropImportJobList from './DropImportJobList.svelte';

function job(overrides: Partial<DropImportJob> = {}): DropImportJob {
	return {
		id: 'job-1',
		status: 'completed',
		created_at: Date.now() / 1000,
		upload_name: 'album.zip',
		user_id: 'user-1',
		user_name: 'Harvey',
		error: null,
		items: [
			{
				id: 1,
				folder_name: 'Artist - Album',
				status: 'imported',
				updated_at: Date.now() / 1000,
				release_group_mbid: 'rg-1',
				album_title: 'Album',
				artist_name: 'Artist',
				files_total: 10,
				files_imported: 10,
				detail: 'Imported 10'
			}
		],
		...overrides
	};
}

describe('DropImportJobList', () => {
	beforeEach(() => {
		h.jobs = [];
		h.isLoading = false;
		h.discard.mockClear();
	});

	it('shows the empty state when nothing was imported yet', async () => {
		render(DropImportJobList);
		await expect.element(page.getByText('Nothing imported yet', { exact: false })).toBeVisible();
	});

	it('renders an imported item with its album identity and detail', async () => {
		h.jobs = [job()];
		render(DropImportJobList);
		await expect.element(page.getByText('Artist - Album')).toBeVisible();
		await expect.element(page.getByText('Imported', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Imported 10')).toBeVisible();
	});

	it('offers Match and Discard on a needs_review item, and discard calls through', async () => {
		h.jobs = [
			job({
				items: [
					{
						id: 2,
						folder_name: 'Mystery Folder',
						status: 'needs_review',
						updated_at: Date.now() / 1000,
						release_group_mbid: null,
						album_title: null,
						artist_name: null,
						files_total: 3,
						files_imported: 0,
						detail: "Couldn't identify this as an album - match it manually"
					}
				]
			})
		];
		render(DropImportJobList);
		await expect.element(page.getByText('Needs a match')).toBeVisible();
		await page.getByRole('button', { name: 'Discard Mystery Folder' }).click();
		expect(h.discard).toHaveBeenCalledWith(2);
	});

	it('opens the match modal from the Match button', async () => {
		h.jobs = [
			job({
				items: [
					{
						id: 3,
						folder_name: 'Mystery Folder',
						status: 'needs_review',
						updated_at: Date.now() / 1000,
						release_group_mbid: null,
						album_title: null,
						artist_name: null,
						files_total: 3,
						files_imported: 0,
						detail: null
					}
				]
			})
		];
		render(DropImportJobList);
		await page.getByRole('button', { name: 'Match…' }).click();
		await expect.element(page.getByText('Match to an album')).toBeVisible();
	});
});
