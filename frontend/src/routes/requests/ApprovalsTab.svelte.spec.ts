import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { RequestItem } from '$lib/queries/requests/types';

const { approvalItems, mutations } = vi.hoisted(() => {
	const approvalItems = [
		{
			album_title: 'Blue Lines',
			artist_mbid: 'artist-mbid',
			artist_name: 'Massive Attack',
			completed_at: null,
			duration_seconds: null,
			musicbrainz_id: 'album-mbid-7',
			request_kind: 'album',
			requested_at: 1700000000,
			requested_by_name: 'Alice',
			requester_count: 0,
			reviewed_by_name: null,
			status: 'pending',
			task_id: null,
			track_title: null,
			user_id: 'user-1',
			year: 1991
		} as RequestItem
	];
	const stub = () => ({ mutateAsync: vi.fn().mockResolvedValue({}), isPending: false });
	return { approvalItems, mutations: { approve: stub(), reject: stub() } };
});

vi.mock('$lib/components/AlbumImage.svelte', () => {
	const C = function () {};
	C.prototype = {};
	return { default: C };
});
vi.mock('$lib/queries/requests/RequestQueries.svelte', () => ({
	getApprovalsQuery: () => ({
		data: { items: approvalItems, count: approvalItems.length },
		isPending: false,
		isError: false,
		refetch: vi.fn()
	})
}));
vi.mock('$lib/queries/requests/RequestMutations.svelte', () => ({
	createApproveRequestMutation: () => mutations.approve,
	createRejectRequestMutation: () => mutations.reject
}));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { isAdmin: true, isTrusted: true, user: { id: 'user-1' } }
}));

import ApprovalsTab from './ApprovalsTab.svelte';

describe('requests Approvals tab', () => {
	it('shows pending approvals with row actions for admins', async () => {
		await render(ApprovalsTab, { active: true });

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
