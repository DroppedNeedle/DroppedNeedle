import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { withBasePath } from '$lib/utils/basePath';
import LaterPile from './LaterPile.svelte';

const PILE = 'GET /api/v3/discover/later';
const REMOVE = 'DELETE /api/v3/discover/later/{release_group_mbid}';
type Row = RouteData<typeof PILE>['items'][number];

const ADDED_AT = Date.UTC(2026, 10, 20, 12) / 1000;

function row(mbid: string, title: string, artist: string, requested = false): Row {
	return {
		album: {
			title,
			musicbrainz_id: mbid,
			artist_name: artist,
			artist_id: `ar-${mbid}`,
			cover_url: `/api/v3/covers/release-group/${mbid}?size=500`,
			in_library: false,
			requested
		},
		added_at: ADDED_AT
	};
}

const ROWS: Row[] = [
	row('rg-1', 'Dreamhouse', 'Tides of Man'),
	row('rg-2', 'Juturna', 'Circa Survive'),
	row('rg-3', 'Rumours', 'Fleetwood Mac', true)
];

function signIn() {
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
}

describe('LaterPile.svelte', () => {
	it('lists the pile, and Remove takes one album off', async () => {
		let items = ROWS;
		const api = fakeApi({
			[PILE]: () => ({ items, next_cursor: null, total: items.length }),
			[REMOVE]: ({ params }) => {
				items = items.filter((later) => later.album.musicbrainz_id !== params.release_group_mbid);
			}
		});
		signIn();
		// The removal's effect sweeps the app's own client.
		await renderWithQuery(LaterPile, {}, { client: queryClient });

		const list = page.getByRole('list');
		await expect.element(page.getByText('3 albums for later')).toBeVisible();
		await expect.element(list.getByRole('link', { name: 'Dreamhouse' })).toBeVisible();
		await expect.element(list.getByText('Circa Survive')).toBeVisible();
		await expect.element(list.getByRole('button', { name: 'Keep Juturna' })).toBeVisible();
		await expect.element(list.getByText('Waiting')).toBeVisible();

		await list.getByRole('button', { name: 'Remove Juturna from your Later pile' }).click();
		await expect.element(list.getByText('Juturna')).not.toBeInTheDocument();
		expect(api.calls(REMOVE).map((call) => call.params)).toEqual([{ release_group_mbid: 'rg-2' }]);
		await expect.element(page.getByText('2 albums for later')).toBeVisible();
	});

	it('opens the batch dialog with the albums still to request', async () => {
		fakeApi({ [PILE]: () => ({ items: ROWS, next_cursor: null, total: ROWS.length }) });
		signIn();
		await renderWithQuery(LaterPile, {});

		await page.getByRole('button', { name: 'Keep all' }).click();
		const dialog = page.getByRole('dialog');
		await expect.element(dialog.getByText('Download this section')).toBeVisible();
		for (const title of ['Dreamhouse', 'Juturna']) {
			await expect.element(dialog.getByText(title)).toBeVisible();
		}
		await expect.element(dialog.getByText('Rumours')).not.toBeInTheDocument();
		await expect.element(dialog.getByRole('button', { name: 'Request 2 albums' })).toBeVisible();
	});

	it('offers one batch of the newest 30 from a larger pile', async () => {
		const many = Array.from({ length: 31 }, (_, n) =>
			row(`rg-${n + 1}`, `Album ${n + 1}`, 'Someone')
		);
		fakeApi({ [PILE]: () => ({ items: many, next_cursor: null, total: many.length }) });
		signIn();
		await renderWithQuery(LaterPile, {});

		await expect.element(page.getByText('Keep all takes the newest 30.')).toBeVisible();
		await page.getByRole('button', { name: 'Keep all' }).click();
		const dialog = page.getByRole('dialog');
		await expect.element(dialog.getByRole('button', { name: 'Request 30 albums' })).toBeVisible();
		await expect.element(dialog.getByText('Album 30', { exact: true })).toBeVisible();
		await expect.element(dialog.getByText('Album 31', { exact: true })).not.toBeInTheDocument();
	});

	it('offers no Keep all when nothing on the pile can be requested', async () => {
		const asked = [ROWS[2]];
		fakeApi({ [PILE]: () => ({ items: asked, next_cursor: null, total: asked.length }) });
		signIn();
		await renderWithQuery(LaterPile, {});

		await expect.element(page.getByText('1 album for later')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Keep all' })).not.toBeInTheDocument();
	});

	it('says when the pile is empty', async () => {
		fakeApi({ [PILE]: () => ({ items: [], next_cursor: null, total: 0 }) });
		signIn();
		await renderWithQuery(LaterPile, {});

		await expect.element(page.getByText('Nothing on your Later pile')).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Back to Discover' }))
			.toHaveAttribute('href', withBasePath('/discover'));
		await expect.element(page.getByRole('button', { name: 'Keep all' })).not.toBeInTheDocument();
	});
});
