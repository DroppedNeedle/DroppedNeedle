import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { RequestItem } from '$lib/queries/requests/types';

const { activeItems, historyItems, approvalItems, mutations, emptyComponent } = vi.hoisted(() => {
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
	const historyItems = [row({ musicbrainz_id: 'album-mbid-9', status: 'imported' })];
	const approvalItems = [row({ musicbrainz_id: 'album-mbid-7', status: 'pending' })];
	const stub = () => ({ mutateAsync: vi.fn().mockResolvedValue({}), isPending: false });
	const mutations = {
		cancel: stub(),
		retry: stub(),
		clear: stub(),
		approve: stub(),
		reject: stub(),
		batchCancel: stub()
	};
	const emptyComponent = () => {
		const C = function () {};
		C.prototype = {};
		return { default: C };
	};
	return { activeItems, historyItems, approvalItems, mutations, emptyComponent };
});

vi.mock('$lib/components/RequestCard.svelte', async () => {
	const harness = await import('./RequestCardTestHarness.svelte');
	return { default: harness.default };
});
vi.mock('$lib/components/WantedWatchCard.svelte', emptyComponent);
vi.mock('$lib/components/WantedRetryingCard.svelte', emptyComponent);
vi.mock('$lib/components/Pagination.svelte', emptyComponent);
vi.mock('$lib/components/Toast.svelte', emptyComponent);
vi.mock('$lib/components/AlbumImage.svelte', emptyComponent);
vi.mock('$lib/components/ArtistImage.svelte', emptyComponent);

vi.mock('$lib/queries/requests/RequestQueries.svelte', () => ({
	getActiveRequestsQuery: () => ({
		data: { items: activeItems, count: activeItems.length },
		isPending: false,
		isError: false,
		isFetching: false,
		refetch: vi.fn()
	}),
	getActiveRequestCountQuery: () => ({ data: { count: activeItems.length } }),
	getRequestHistoryQuery: () => ({
		data: { items: historyItems, total: historyItems.length, total_pages: 1 },
		isPending: false,
		isError: false,
		refetch: vi.fn()
	}),
	getApprovalsQuery: () => ({
		data: { items: approvalItems, count: approvalItems.length },
		isPending: false,
		isError: false,
		refetch: vi.fn()
	})
}));
vi.mock('$lib/queries/requests/RequestMutations.svelte', () => ({
	createCancelRequestMutation: () => mutations.cancel,
	createRetryRequestMutation: () => mutations.retry,
	createClearHistoryMutation: () => mutations.clear,
	createApproveRequestMutation: () => mutations.approve,
	createRejectRequestMutation: () => mutations.reject,
	createBatchCancelRequestsMutation: () => mutations.batchCancel
}));

vi.mock('$lib/queries/wanted/WantedQuery.svelte', () => ({
	getWantedWatchesQuery: () => ({ data: { items: [], retrying: [] } })
}));
vi.mock('$lib/queries/wanted/WantedMutations.svelte', () => ({
	createStopWatchMutation: () => ({ mutate: vi.fn(), isPending: false }),
	createResumeWatchMutation: () => ({ mutate: vi.fn(), isPending: false }),
	createMarkWantedSeenMutation: () => ({ mutate: vi.fn(), isPending: false })
}));
vi.mock('$lib/queries/following/AdminApprovalsQueries.svelte', () => ({
	getAutoDownloadApprovalsQuery: () => ({ data: { items: [], count: 0 } }),
	getAutoDownloadApprovalBatchesQuery: () => ({ data: { batches: [], count: 0 } })
}));
vi.mock('$lib/queries/following/AdminApprovalsMutations.svelte', () => ({
	createApproveAutoDownloadMutation: () => ({ mutate: vi.fn(), isPending: false }),
	createRejectAutoDownloadMutation: () => ({ mutate: vi.fn(), isPending: false }),
	createApproveAutoDownloadBatchMutation: () => ({ mutate: vi.fn(), isPending: false }),
	createRejectAutoDownloadBatchMutation: () => ({ mutate: vi.fn(), isPending: false })
}));
vi.mock('$lib/queries/scrobble-preferences/PersonalMixApprovalsQuery.svelte', () => ({
	getPersonalMixApprovalsQuery: () => ({ data: { items: [], count: 0 } })
}));
vi.mock('$lib/queries/scrobble-preferences/ScrobblePreferencesMutations.svelte', () => ({
	createApprovePersonalMixMutation: () => ({ mutate: vi.fn(), isPending: false }),
	createRejectPersonalMixMutation: () => ({ mutate: vi.fn(), isPending: false })
}));
vi.mock('$lib/queries/downloads/UpgradeQueries.svelte', () => ({
	getCutoffUnmetQuery: () => ({ data: undefined, isPending: false, isError: false }),
	requestUpgradeAlbum: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { isAdmin: true, isTrusted: true, user: { id: 'user-1' } }
}));
vi.mock('$app/state', () => ({ page: { url: new URL('http://localhost/requests') } }));

import RequestsPage from './+page.svelte';

describe('requests route page (R10 surface)', () => {
	it('lists the active requests with a live count badge', async () => {
		await render(RequestsPage);

		await expect.element(page.getByText('Blue Lines').first()).toBeInTheDocument();
		await expect.element(page.getByText('Unfinished Sympathy').first()).toBeInTheDocument();
		const badge = page.getByRole('tab', { name: /Active/ });
		await expect.element(badge.getByText('2')).toBeInTheDocument();
	});

	it('batch-cancels the selected rows with one call per kind', async () => {
		await render(RequestsPage);

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

	it('reads history through the v3 history hook when the tab opens', async () => {
		await render(RequestsPage);

		await page.getByRole('tab', { name: /History/ }).click();

		await expect
			.element(page.getByRole('combobox', { name: 'Filter by status' }))
			.toBeInTheDocument();
		await expect.element(page.getByText('Blue Lines').first()).toBeInTheDocument();
	});

	it('shows pending approvals with row actions for admins', async () => {
		await render(RequestsPage);

		await page.getByRole('tab', { name: /Approvals/ }).click();

		await expect.element(page.getByText('Blue Lines').first()).toBeInTheDocument();
		const approve = page.getByRole('button', { name: 'Approve' });
		await expect.element(approve).toBeInTheDocument();
		await approve.click();
		await vi.waitFor(() =>
			expect(mutations.approve.mutateAsync).toHaveBeenCalledWith({
				mbid: 'album-mbid-7',
				kind: 'album'
			})
		);
	});
});
