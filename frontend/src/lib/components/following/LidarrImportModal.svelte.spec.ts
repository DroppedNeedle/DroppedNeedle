import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { LidarrArtistList } from '$lib/queries/lidarr-import/types';

vi.mock('$env/dynamic/public', () => ({
	env: { PUBLIC_API_URL: '' }
}));

const CANDIDATES: LidarrArtistList = {
	total: 3,
	artists: [
		{
			mbid: '11111111-1111-1111-1111-111111111111',
			name: 'Radiohead',
			monitor_new_items: 'all',
			already_following: false,
			would_auto_download: true
		},
		{
			mbid: '22222222-2222-2222-2222-222222222222',
			name: 'Steely Dan',
			monitor_new_items: 'none',
			already_following: false,
			would_auto_download: false
		},
		{
			mbid: '33333333-3333-3333-3333-333333333333',
			name: 'Boards of Canada',
			monitor_new_items: 'none',
			already_following: true,
			would_auto_download: false
		}
	]
};

const mutateAsync = vi.fn();

vi.mock('$lib/queries/lidarr-import/LidarrImportQueries.svelte', () => ({
	getLidarrImportCandidatesQuery: () => ({
		data: CANDIDATES,
		isPending: false,
		isError: false
	})
}));

vi.mock('$lib/queries/lidarr-import/LidarrImportMutations.svelte', () => ({
	importFromLidarrMutation: () => ({ mutateAsync, isPending: false })
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { isAdmin: false, user: { id: 'u1' } }
}));

import LidarrImportModal from './LidarrImportModal.svelte';

function renderOpen() {
	return render(LidarrImportModal, {
		props: { open: true }
	} as Parameters<typeof render<typeof LidarrImportModal>>[1]);
}

describe('LidarrImportModal', () => {
	beforeEach(() => {
		mutateAsync.mockReset();
		mutateAsync.mockResolvedValue({
			imported: 2,
			already_following: 1,
			skipped_invalid: 0,
			auto_download_enabled: 1,
			approval_batch_id: 'batch-1'
		});
	});

	it('renders every monitored artist with an auto-download badge where applicable', async () => {
		renderOpen();
		await expect.element(page.getByText('Radiohead')).toBeVisible();
		await expect.element(page.getByText('Steely Dan')).toBeVisible();
		await expect.element(page.getByText('Boards of Canada')).toBeVisible();
		await expect.element(page.getByText('Auto-download')).toBeVisible();
	});

	it('pre-checks not-yet-followed rows and disables already-following ones', async () => {
		renderOpen();
		// D7: two not-yet-followed rows selected by default.
		await expect.element(page.getByText('2 of 2 selected')).toBeVisible();
		await expect.element(page.getByText('Following')).toBeVisible();
	});

	it('imports the selected MBIDs and renders the result summary', async () => {
		renderOpen();
		await page.getByRole('button', { name: /Import 2/ }).click();
		expect(mutateAsync).toHaveBeenCalledWith([
			'11111111-1111-1111-1111-111111111111',
			'22222222-2222-2222-2222-222222222222'
		]);
		await expect
			.element(page.getByText('2 imported · 1 already following · 0 skipped'))
			.toBeVisible();
		// Non-admin: the auto-download note points to pending admin approval.
		await expect
			.element(page.getByText(/pending admin approval/))
			.toBeVisible();
	});
});
