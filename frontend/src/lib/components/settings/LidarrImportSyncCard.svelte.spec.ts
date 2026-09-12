import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { LidarrArtistList } from '$lib/queries/lidarr-import/types';

vi.mock('$env/dynamic/public', () => ({
	env: { PUBLIC_API_URL: '' }
}));

const h = vi.hoisted(() => ({
	data: undefined as LidarrArtistList | undefined,
	isPending: false,
	isError: false,
	refetch: vi.fn(),
	mutateAsync: vi.fn(),
	isMutating: false,
	capturedEnabled: null as (() => boolean) | null
}));

vi.mock('$lib/queries/lidarr-import/LidarrImportQueries.svelte', () => ({
	getLidarrImportCandidatesQuery: (getEnabled: () => boolean) => {
		h.capturedEnabled = getEnabled;
		return {
			get data() {
				return h.data;
			},
			get isPending() {
				return h.isPending;
			},
			get isError() {
				return h.isError;
			},
			refetch: (...args: unknown[]) => h.refetch(...args)
		};
	}
}));

vi.mock('$lib/queries/lidarr-import/LidarrImportMutations.svelte', () => ({
	importFromLidarrMutation: () => ({
		mutateAsync: (...args: unknown[]) => h.mutateAsync(...args),
		get isPending() {
			return h.isMutating;
		}
	})
}));

import LidarrImportSyncCard from './LidarrImportSyncCard.svelte';

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

const ALL_FOLLOWING: LidarrArtistList = {
	total: 2,
	artists: [
		{
			mbid: '11111111-1111-1111-1111-111111111111',
			name: 'Radiohead',
			monitor_new_items: 'all',
			already_following: true,
			would_auto_download: false
		},
		{
			mbid: '22222222-2222-2222-2222-222222222222',
			name: 'Steely Dan',
			monitor_new_items: 'none',
			already_following: true,
			would_auto_download: false
		}
	]
};

describe('LidarrImportSyncCard', () => {
	beforeEach(() => {
		h.data = CANDIDATES;
		h.isPending = false;
		h.isError = false;
		h.isMutating = false;
		h.capturedEnabled = null;
		h.refetch.mockReset();
		h.mutateAsync.mockReset();
		h.mutateAsync.mockResolvedValue({
			imported: 2,
			already_following: 1,
			skipped_invalid: 0,
			auto_download_enabled: 1,
			approval_batch_id: null
		});
	});

	it('renders the locked panel when unlocked=false and leaves the candidates query disabled', async () => {
		await render(LidarrImportSyncCard, { props: { unlocked: false } } as Parameters<
			typeof render<typeof LidarrImportSyncCard>
		>[1]);
		await expect
			.element(page.getByText('Connect Lidarr above to unlock artist sync.'))
			.toBeVisible();
		expect(h.capturedEnabled?.()).toBe(false);
	});

	it('shows unsynced counts and an Import K button with K new', async () => {
		await render(LidarrImportSyncCard, { props: { unlocked: true } } as Parameters<
			typeof render<typeof LidarrImportSyncCard>
		>[1]);
		await expect.element(page.getByText('1 of 3 in your follows · 2 new')).toBeVisible();
		await expect.element(page.getByRole('button', { name: /Import 2/ })).toBeVisible();
		expect(h.capturedEnabled?.()).toBe(true);
	});

	it('pre-checks every not-yet-followed row (D7)', async () => {
		await render(LidarrImportSyncCard, { props: { unlocked: true } } as Parameters<
			typeof render<typeof LidarrImportSyncCard>
		>[1]);
		// D7: two not-yet-followed rows selected by default.
		await expect.element(page.getByText('2 of 2 selected')).toBeVisible();
	});

	it('shows the in-sync banner when all monitored artists are followed and Check again refetches', async () => {
		h.data = ALL_FOLLOWING;
		await render(LidarrImportSyncCard, { props: { unlocked: true } } as Parameters<
			typeof render<typeof LidarrImportSyncCard>
		>[1]);
		await expect
			.element(page.getByText('All 2 monitored artists are in your follows.'))
			.toBeVisible();
		await page.getByRole('button', { name: 'Check again' }).click();
		expect(h.refetch).toHaveBeenCalled();
	});

	it('renders the result summary after the import resolves', async () => {
		await render(LidarrImportSyncCard, { props: { unlocked: true } } as Parameters<
			typeof render<typeof LidarrImportSyncCard>
		>[1]);
		await page.getByRole('button', { name: /Import 2/ }).click();
		expect(h.mutateAsync).toHaveBeenCalledWith([
			'11111111-1111-1111-1111-111111111111',
			'22222222-2222-2222-2222-222222222222'
		]);
		await expect
			.element(page.getByText('2 imported · 1 already following · 0 skipped'))
			.toBeVisible();
		await expect.element(page.getByText('Auto-download enabled for 1 artists.')).toBeVisible();
	});

	it('shows an error alert with a Retry button that refetches', async () => {
		h.isError = true;
		await render(LidarrImportSyncCard, { props: { unlocked: true } } as Parameters<
			typeof render<typeof LidarrImportSyncCard>
		>[1]);
		await expect.element(page.getByText(/Couldn't reach Lidarr/)).toBeVisible();
		await page.getByRole('button', { name: 'Retry' }).click();
		expect(h.refetch).toHaveBeenCalled();
	});

	it('exits the result state via Select remaining after a partial import', async () => {
		await render(LidarrImportSyncCard, { props: { unlocked: true } } as Parameters<
			typeof render<typeof LidarrImportSyncCard>
		>[1]);
		await page.getByRole('button', { name: /Import 2/ }).click();
		await expect
			.element(page.getByText('2 imported · 1 already following · 0 skipped'))
			.toBeVisible();
		await page.getByRole('button', { name: 'Select remaining' }).click();
		await expect.element(page.getByText('2 of 2 selected')).toBeVisible();
	});

	it('shows an empty state with a Check again button that refetches', async () => {
		h.data = { total: 0, artists: [] };
		await render(LidarrImportSyncCard, { props: { unlocked: true } } as Parameters<
			typeof render<typeof LidarrImportSyncCard>
		>[1]);
		await expect.element(page.getByText('No monitored artists in Lidarr.')).toBeVisible();
		await page.getByRole('button', { name: 'Check again' }).click();
		expect(h.refetch).toHaveBeenCalled();
	});
});
