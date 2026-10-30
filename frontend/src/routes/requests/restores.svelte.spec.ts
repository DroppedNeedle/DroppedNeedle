import { page, userEvent } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { RequestItem } from '$lib/queries/requests/types';
import type { WantedRetryingItem, WantedWatchItem } from '$lib/queries/wanted/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi, type FakeRoutes } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

import ApprovalsTab from './ApprovalsTab.svelte';
import HistoryTab from './HistoryTab.svelte';
import WantedTab from './WantedTab.svelte';

// The lazy loader would fetch the cover, and a failed fetch removes the image.
// With it off, the cover keeps the URL it would fetch in data-src.
vi.mock('$lib/utils/lazyImage', async (importOriginal) => ({
	...(await importOriginal<Record<string, unknown>>()),
	lazyImage: () => ({ destroy() {} })
}));

const now = Math.floor(Date.now() / 1000);

const watch: WantedWatchItem = {
	musicbrainz_id: 'rg-blue-lines',
	album_title: 'Blue Lines',
	artist_name: 'Massive Attack',
	kind: 'missing',
	state: 'watching',
	check_count: 2,
	created_at: now - 86400,
	next_check_at: now + 3600,
	new_candidate_count: 0,
	year: 1991
};

const retrying: WantedRetryingItem = {
	musicbrainz_id: 'rg-collected',
	album_title: 'Collected',
	artist_name: 'Massive Attack',
	max_attempts: 5,
	retry_count: 2,
	next_retry_at: now + 1800
};

const trackApproval: RequestItem = {
	musicbrainz_id: 'recording-angel',
	request_kind: 'track',
	track_title: 'Angel',
	track_release_group_mbid: 'rg-mezzanine',
	album_title: 'Mezzanine',
	artist_name: 'Massive Attack',
	origin: 'user',
	requested_at: now - 3600,
	requester_count: 0,
	status: 'awaiting_approval'
};

const emptyHistory: FakeRoutes = {
	'GET /api/v3/requests/history': { items: [], total: 0, page: 1, page_size: 20, total_pages: 1 },
	'GET /api/v3/requests/wanted': { count: 0, items: [], retrying: [] }
};

function signIn(role: 'admin' | 'user') {
	authStore.setUser({
		id: role === 'admin' ? 'admin-1' : 'user-1',
		display_name: role,
		role,
		email: null,
		avatar_url: null,
		username: role,
		username_display: role,
		providers: []
	});
}

describe('requests restores', () => {
	afterEach(() => authStore.clear());

	it('shows whose watch each wanted card is to an admin', async () => {
		signIn('admin');
		fakeApi({
			'GET /api/v3/requests/wanted': {
				count: 1,
				items: [{ ...watch, user_id: 'user-2', user_name: 'Alice' }],
				retrying: [{ ...retrying, user_id: 'user-2', user_name: 'Alice' }]
			}
		});
		await renderWithQuery(WantedTab, { active: true });

		await expect.element(page.getByText('watched for Alice')).toBeInTheDocument();
		await expect.element(page.getByText('requested by Alice')).toBeInTheDocument();
	});

	it('lets a user stop their own watch', async () => {
		signIn('user');
		fakeApi({
			'GET /api/v3/requests/wanted': { count: 1, items: [watch], retrying: [] }
		});
		await renderWithQuery(WantedTab, { active: true });

		await expect.element(page.getByRole('button', { name: 'Stop' })).toBeInTheDocument();
	});

	it('shows the album cover on a track approval', async () => {
		signIn('admin');
		fakeApi({
			'GET /api/v3/requests/approvals': { count: 1, items: [trackApproval] }
		});
		await renderWithQuery(ApprovalsTab, { active: true });

		await expect
			.element(page.getByRole('img', { name: 'Mezzanine' }))
			.toHaveAttribute('data-src', expect.stringContaining('rg-mezzanine'));
	});

	it('lets an admin list the requests that can be reimported', async () => {
		signIn('admin');
		const api = fakeApi(emptyHistory);
		await renderWithQuery(HistoryTab, { active: true });

		await userEvent.selectOptions(
			page.getByRole('combobox', { name: 'Filter by status' }),
			'reimportable'
		);
		await expect
			.poll(() => api.calls('GET /api/v3/requests/history').at(-1)?.query.get('status'))
			.toBe('reimportable');
	});

	it('offers the reimport filter to admins only', async () => {
		signIn('user');
		fakeApi(emptyHistory);
		await renderWithQuery(HistoryTab, { active: true });

		await expect
			.element(page.getByRole('combobox', { name: 'Filter by status' }))
			.toBeInTheDocument();
		await expect
			.element(page.getByRole('option', { name: 'Can reimport' }))
			.not.toBeInTheDocument();
	});
});
