import { get } from 'svelte/store';
import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it } from 'vitest';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { toAuthUser } from '$lib/features/auth/queries/types';
import type { RequestItem } from '$lib/features/requests/queries/types';

import ActiveTab from './ActiveTab.svelte';
import HistoryTab from './HistoryTab.svelte';

const ALBUM = '1b2c3d4e-0000-4000-8000-000000000091';

// A request another person made, which the signed-in user joined.
function joined(over: Partial<RequestItem>): RequestItem {
	return {
		musicbrainz_id: ALBUM,
		artist_name: 'Massive Attack',
		album_title: 'Blue Lines',
		requested_at: 1_700_000_000,
		status: 'downloading',
		request_kind: 'album',
		origin: 'user',
		user_id: 'owner-1',
		requested_by_name: 'Owner',
		requester_count: 1,
		...over
	};
}

function history(items: RequestItem[]) {
	return { items, page: 1, page_size: 20, total: items.length, total_pages: 1 };
}

beforeEach(() => {
	localStorage.clear();
	toastStore.hide();
	authStore.setUser(
		toAuthUser({
			id: 'joiner-1',
			display_name: 'Joiner',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'joiner',
			username_display: 'Joiner'
		})
	);
});

describe('a request the user joined', () => {
	it('cancels only their own ask from the Active tab', async () => {
		const api = fakeApi({
			'GET /api/v3/requests/active': { items: [joined({})], count: 1 },
			'DELETE /api/v3/requests/active/{musicbrainz_id}': {
				success: true,
				message: 'Removed your request'
			}
		});
		await renderWithQuery(ActiveTab, { active: true });

		await page.getByRole('button', { name: 'Cancel my request' }).click();
		await page.getByRole('button', { name: 'Yes' }).click();

		await expect
			.poll(() => api.calls('DELETE /api/v3/requests/active/{musicbrainz_id}').length)
			.toBe(1);
		const [call] = api.calls('DELETE /api/v3/requests/active/{musicbrainz_id}');
		expect(call?.params).toEqual({ musicbrainz_id: ALBUM });
		expect(call?.query.get('kind')).toBe('album');
	});

	it('retries a failed request from the History tab', async () => {
		const api = fakeApi({
			'GET /api/v3/requests/history': history([joined({ status: 'failed' })]),
			'GET /api/v3/requests/wanted': { items: [], retrying: [], count: 0 },
			'POST /api/v3/requests/retry/{musicbrainz_id}': {
				success: true,
				message: 'Request retried'
			}
		});
		await renderWithQuery(HistoryTab, { active: true });

		await page.getByRole('button', { name: 'Retry request' }).click();

		await expect
			.poll(() => api.calls('POST /api/v3/requests/retry/{musicbrainz_id}').length)
			.toBe(1);
		expect(api.calls('POST /api/v3/requests/retry/{musicbrainz_id}')[0]?.params).toEqual({
			musicbrainz_id: ALBUM
		});
	});
});

describe('clearing a request from history', () => {
	it('says so when the server refuses the clear', async () => {
		fakeApi({
			'GET /api/v3/requests/history': history([joined({ status: 'rejected' })]),
			'GET /api/v3/requests/wanted': { items: [], retrying: [], count: 0 },
			'DELETE /api/v3/requests/history/{musicbrainz_id}': { success: false }
		});
		await renderWithQuery(HistoryTab, { active: true });

		await page.getByRole('button', { name: 'Clear from history' }).click();

		await expect
			.poll(() => get(toastStore))
			.toMatchObject({ type: 'error', message: "That request can't be removed from history" });
	});
});
