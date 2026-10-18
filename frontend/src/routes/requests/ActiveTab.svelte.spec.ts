import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { RequestItem } from '$lib/queries/requests/types';

const { activeItems, mutations } = vi.hoisted(() => {
	const row = (overrides: Partial<RequestItem>): RequestItem =>
		({
			album_title: 'Blue Lines',
			artist_mbid: 'artist-mbid',
			artist_name: 'Massive Attack',
			completed_at: null,
			duration_seconds: null,
			musicbrainz_id: 'album-mbid-1',
			request_kind: 'album',
			requested_at: 1700000000,
			requested_by_name: 'Alice',
			requester_count: 0,
			reviewed_by_name: null,
			status: 'downloading',
			task_id: null,
			track_title: null,
			user_id: 'user-1',
			year: 1991,
			...overrides
		}) as RequestItem;
	const activeItems = [
		row({}),
		row({
			musicbrainz_id: 'track-mbid-2',
			request_kind: 'track',
			track_title: 'Unfinished Sympathy',
			status: 'pending'
		})
	];
	const stub = () => ({ mutateAsync: vi.fn().mockResolvedValue({}), isPending: false });
	return { activeItems, mutations: { cancel: stub(), batchCancel: stub() } };
});

vi.mock('$lib/components/RequestCard.svelte', async () => {
	const harness = await import('./RequestCardTestHarness.svelte');
	return { default: harness.default };
});
vi.mock('$lib/queries/requests/RequestQueries.svelte', () => ({
	getActiveRequestsQuery: () => ({
		data: { items: activeItems, count: activeItems.length },
		isPending: false,
		isError: false,
		isFetching: false,
		refetch: vi.fn()
	})
}));
vi.mock('$lib/queries/downloads/DownloadQueries.svelte', () => ({
	getRequestDownloadsQuery: () => ({ data: undefined })
}));
vi.mock('$lib/queries/requests/RequestMutations.svelte', () => ({
	createCancelRequestMutation: () => mutations.cancel,
	createBatchCancelRequestsMutation: () => mutations.batchCancel
}));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { isAdmin: true, isTrusted: true, user: { id: 'user-1' } }
}));

import ActiveTab from './ActiveTab.svelte';

describe('requests Active tab', () => {
	it('batch-cancels the selected rows with one call per kind', async () => {
		await render(ActiveTab, { active: true });

		await page.getByRole('checkbox', { name: 'Select Blue Lines' }).click();
		await page.getByRole('checkbox', { name: 'Select Unfinished Sympathy' }).click();
		await expect.element(page.getByText('2 selected')).toBeInTheDocument();
		await page.getByRole('button', { name: 'Cancel selected' }).click();

		await vi.waitFor(() =>
			expect(mutations.batchCancel.mutateAsync).toHaveBeenCalledWith({
				mbids: ['album-mbid-1'],
				kind: 'album'
			})
		);
		expect(mutations.batchCancel.mutateAsync).toHaveBeenCalledWith({
			mbids: ['track-mbid-2'],
			kind: 'track'
		});
	});
});
