import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import ToldDiscoverModal from './ToldDiscoverModal.svelte';

const LIST = 'GET /api/v3/discover/feedback';
const UNDO = 'DELETE /api/v3/discover/feedback/{target}/{mbid}';
type Entry = RouteData<typeof LIST>['items'][number];

const TOLD_AT = Date.UTC(2026, 10, 22, 12) / 1000;
const BACK_AT = Date.UTC(2027, 1, 22, 12) / 1000;
const day = (seconds: number) =>
	new Date(seconds * 1000).toLocaleDateString(undefined, { day: 'numeric', month: 'short' });

function entry(told: Partial<Entry> & Pick<Entry, 'kind' | 'target_mbid'>): Entry {
	return {
		target: 'album',
		release_group_mbid: told.target_mbid,
		artist_mbid: '',
		release_name: '',
		artist_name: '',
		created_at: TOLD_AT,
		expires_at: null,
		...told
	};
}

const ENTRIES: Entry[] = [
	entry({ kind: 'no', target_mbid: 'rg-1', release_name: 'Dreamhouse', artist_name: 'Tides' }),
	entry({ kind: 'know_it', target_mbid: 'rg-2', release_name: 'Rumours' }),
	entry({ kind: 'not_now', target_mbid: 'rg-3', release_name: 'Later', expires_at: BACK_AT }),
	entry({
		kind: 'not_artist',
		target: 'artist',
		target_mbid: 'ar-1',
		release_group_mbid: '',
		artist_name: 'Tides of Man'
	})
];

describe('ToldDiscoverModal.svelte', () => {
	it('lists each kind of answer in words, when it lapses, and undoes one', async () => {
		let items = ENTRIES;
		const api = fakeApi({
			[LIST]: () => ({ items }),
			[UNDO]: ({ params }) => {
				items = items.filter((told) => told.target_mbid !== params.mbid);
			}
		});
		authStore.setUser(
			toAuthUser({
				id: 'u1',
				display_name: 'Ada',
				role: 'user',
				email: null,
				avatar_url: null,
				username: 'ada',
				username_display: 'ada'
			})
		);
		// The undo's effect sweeps the app's own client.
		await renderWithQuery(ToldDiscoverModal, { open: true }, { client: queryClient });

		const told = day(TOLD_AT);
		await expect.element(page.getByText(`Passed · ${told} · Until you undo it`)).toBeVisible();
		await expect
			.element(page.getByText(`Know it already · ${told} · Until you undo it`))
			.toBeVisible();
		await expect
			.element(page.getByText(`Not now · ${told} · Back after ${day(BACK_AT)}`))
			.toBeVisible();
		await expect
			.element(page.getByText(`Not this artist · ${told} · Until you undo it`))
			.toBeVisible();
		await expect.element(page.getByText('Tides of Man')).toBeVisible();

		await page.getByRole('button', { name: 'Undo for Tides of Man' }).click();
		await expect.element(page.getByText('Tides of Man')).not.toBeInTheDocument();
		expect(api.calls(UNDO).map((call) => call.params)).toEqual([
			{ target: 'artist', mbid: 'ar-1' }
		]);
		await expect.element(page.getByText('Dreamhouse')).toBeVisible();
	});
});
