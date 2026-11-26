import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import PastWeeksRow from './PastWeeksRow.svelte';

const WEEKS = 'GET /api/v3/discover/weeks';
type Week = RouteData<typeof WEEKS>['items'][number];
type Pick = Week['picks'][number];

const DAY = 86_400_000;
const THIS_WEEK = Date.UTC(2026, 10, 20);
const LAST_WEEK = THIS_WEEK - 7 * DAY;

function pick(n: number, state: Pick['state'], plays: number | null = null): Pick {
	return {
		pick_no: n,
		album: {
			title: `Album ${n}`,
			musicbrainz_id: `rg-${n}`,
			artist_name: `Artist ${n}`,
			in_library: false,
			requested: false
		},
		reason: { kind: 'your_artists', line: `Close to Seed ${n}.`, artists: [] },
		state,
		arrived_at: null,
		plays
	};
}

function week(at: number, number: number, picks: Pick[]): Week {
	return {
		week_start_at: at,
		week_number: number,
		tally: { kept: 1, later: 1, known: 1, passed: 0, open: picks.length - 3 },
		picks
	};
}

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

describe('PastWeeksRow.svelte', () => {
	it('shows the newest week before the one on the counter, every state a word', async () => {
		const last = [
			pick(1, 'arrived', 14),
			pick(2, 'later'),
			pick(3, 'known'),
			...[4, 5, 6, 7, 8].map((n) => pick(n, 'open'))
		];
		fakeApi({
			[WEEKS]: () => ({
				items: [week(THIS_WEEK, 47, [pick(9, 'open')]), week(LAST_WEEK, 46, last)],
				next_cursor: null,
				total: null,
				record: null
			})
		});
		signIn();
		await renderWithQuery(PastWeeksRow, { current: THIS_WEEK });

		const shelf = page.getByRole('list', { name: 'Week 46' });
		await expect.element(shelf).toBeVisible();
		expect(shelf.getByRole('listitem').all()).toHaveLength(8);
		await expect.element(page.getByText('1 kept - 1 later - 1 known - 5 open')).toBeVisible();
		await expect
			.element(shelf.getByRole('button', { name: 'Album 1 by Artist 1, Kept, Played 14 times' }))
			.toBeVisible();
		await expect
			.element(shelf.getByRole('button', { name: 'Album 2 by Artist 2, Later' }))
			.toBeVisible();
		await expect
			.element(shelf.getByRole('button', { name: 'Album 3 by Artist 3, Know it' }))
			.toBeVisible();
		expect(page.getByText(/Played/).all()).toHaveLength(1);
		await expect.element(page.getByRole('list', { name: 'Week 47' })).not.toBeInTheDocument();
		await expect.element(page.getByRole('link', { name: /All weeks/ })).toBeVisible();

		// Opening a record shows it as it was.
		await shelf.getByRole('button', { name: /Album 2 by/ }).click();
		const opened = page.getByRole('dialog', { name: 'Album 2' });
		await expect.element(opened.getByText('Close to Seed 2.')).toBeVisible();
		await expect.element(opened.getByText('On your Later pile.')).toBeVisible();
	});

	it('says where past weeks will be kept before there is one', async () => {
		fakeApi({
			[WEEKS]: () => ({
				items: [week(THIS_WEEK, 47, [pick(1, 'open')])],
				next_cursor: null,
				total: null,
				record: null
			})
		});
		signIn();
		await renderWithQuery(PastWeeksRow, { current: THIS_WEEK });

		await expect.element(page.getByText('Past weeks will be kept here.')).toBeVisible();
	});
});
