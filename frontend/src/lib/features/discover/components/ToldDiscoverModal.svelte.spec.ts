import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { apiError, fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { dayMonth, dayMonthYear } from '$lib/features/discover/state/weekWords';
import ToldDiscoverModal from './ToldDiscoverModal.svelte';

const LIST = 'GET /api/v3/discover/feedback';
const UNDO = 'DELETE /api/v3/discover/feedback/{target}/{mbid}';
const TUNINGS = 'GET /api/v3/discover/tunings';
const UNDO_TUNING = 'DELETE /api/v3/discover/tunings/{kind}/{target}';
const STATED = 'GET /api/v3/discover/stated-artists';
type Entry = RouteData<typeof LIST>['items'][number];
type Tuning = RouteData<typeof TUNINGS>['items'][number];

const TOLD_AT = Date.UTC(2026, 10, 22, 12) / 1000;
const BACK_AT = Date.UTC(2027, 1, 22, 12) / 1000;
const ENDS_AT = Date.UTC(2027, 4, 1, 12) / 1000;
const day = (seconds: number) => dayMonth(seconds * 1000);

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
	entry({ kind: 'no', target_mbid: 'rg-4', release_name: 'Carried', expires_at: ENDS_AT }),
	entry({ kind: 'not_now', target_mbid: 'rg-3', release_name: 'Later', expires_at: BACK_AT }),
	entry({
		kind: 'not_artist',
		target: 'artist',
		target_mbid: 'ar-1',
		release_group_mbid: '',
		artist_name: 'Tides of Man'
	})
];

const TUNED: Tuning[] = [
	{
		kind: 'less_from_seed',
		target: 'ar-2',
		name: 'Circa Survive',
		group_anchor_mbid: null,
		group_label: null,
		created_at: TOLD_AT
	}
];

describe('ToldDiscoverModal.svelte', () => {
	it('lists each kind of answer in words, when it lapses, and undoes one', async () => {
		let items = ENTRIES;
		let tunings = TUNED;
		const api = fakeApi({
			[LIST]: () => ({ items }),
			[UNDO]: ({ params }) => {
				items = items.filter((told) => told.target_mbid !== params.mbid);
			},
			[TUNINGS]: () => ({ items: tunings }),
			[UNDO_TUNING]: ({ params }) => {
				tunings = tunings.filter((tuning) => tuning.target !== params.target);
			},
			[STATED]: {
				items: [{ artist_mbid: 'ar-9', artist_name: 'Thrice', stated_at: TOLD_AT }],
				limit: 30
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

		const dialog = page.getByRole('dialog', { name: 'What you told Discover' });
		await expect.element(dialog.getByRole('heading', { name: 'Artists you love 1' })).toBeVisible();
		await expect.element(dialog.getByRole('button', { name: 'Remove Thrice' })).toBeVisible();
		await expect.element(dialog.getByRole('heading', { name: 'Not this artist 1' })).toBeVisible();
		await expect.element(dialog.getByRole('heading', { name: 'Less from this 1' })).toBeVisible();
		await expect.element(dialog.getByRole('heading', { name: 'Not now 1' })).toBeVisible();
		await expect.element(dialog.getByRole('heading', { name: 'Know it already 1' })).toBeVisible();
		await expect.element(dialog.getByRole('heading', { name: 'No 2' })).toBeVisible();
		await expect.element(dialog.getByText(`Back after ${day(BACK_AT)}`)).toBeVisible();
		await expect.element(dialog.getByText(`Ends ${dayMonthYear(ENDS_AT * 1000)}`)).toBeVisible();
		await expect.element(dialog.getByText(`Since ${day(TOLD_AT)}`).first()).toBeVisible();
		await expect.element(dialog.getByText('Tides of Man')).toBeVisible();
		await expect
			.element(dialog.getByText('Albums you put on Later are on their own page.'))
			.toBeVisible();

		await page.getByRole('button', { name: 'Undo for Tides of Man' }).click();
		await expect.element(page.getByText('Tides of Man')).not.toBeInTheDocument();
		expect(api.calls(UNDO).map((call) => call.params)).toEqual([
			{ target: 'artist', mbid: 'ar-1' }
		]);
		await expect.element(page.getByText('Dreamhouse')).toBeVisible();

		await page.getByRole('button', { name: 'Undo less from Circa Survive' }).click();
		await expect.element(page.getByText('Circa Survive')).not.toBeInTheDocument();
		expect(api.calls(UNDO_TUNING).map((call) => call.params)).toEqual([
			{ kind: 'less_from_seed', target: 'ar-2' }
		]);
	});

	it('gathers a group statement on one row, and its undo takes back the group', async () => {
		const grouped = (target: string, name: string): Tuning => ({
			kind: 'phase',
			target,
			name,
			group_anchor_mbid: 'ar-5',
			group_label: 'Deep Purple',
			created_at: TOLD_AT
		});
		let tunings: Tuning[] = [grouped('ar-5', 'Deep Purple'), grouped('ar-6', 'Cream')];
		const api = fakeApi({
			[LIST]: { items: [] },
			[TUNINGS]: () => ({ items: tunings }),
			[UNDO_TUNING]: ({ query }) => {
				if (query.get('group') === 'true') tunings = [];
			},
			[STATED]: { items: [], limit: 30 }
		});
		await renderWithQuery(ToldDiscoverModal, { open: true }, { client: queryClient });

		const dialog = page.getByRole('dialog', { name: 'What you told Discover' });
		await expect.element(dialog.getByRole('heading', { name: 'That was a phase 1' })).toBeVisible();
		await expect.element(dialog.getByText('Around Deep Purple')).toBeVisible();
		await expect.element(dialog.getByText('Deep Purple, Cream')).toBeVisible();

		await page.getByRole('button', { name: 'Undo the phase for Around Deep Purple' }).click();
		await expect.element(page.getByText('Around Deep Purple')).not.toBeInTheDocument();
		expect(
			api.calls(UNDO_TUNING).map((call) => [call.params.kind, call.query.get('group')])
		).toEqual([['phase', 'true']]);
	});

	it('says when one of the two lists did not load, and keeps the other', async () => {
		fakeApi({
			[LIST]: () => ({ items: ENTRIES.slice(0, 1) }),
			[TUNINGS]: () => apiError(500, 'INTERNAL_ERROR'),
			[STATED]: { items: [], limit: 30 }
		});
		await renderWithQuery(ToldDiscoverModal, { open: true });

		await expect.element(page.getByText('Dreamhouse')).toBeVisible();
		await expect.element(page.getByText("how you tuned Discover didn't load")).toBeVisible();
		await expect.element(page.getByText('Nothing yet.')).not.toBeInTheDocument();
	});
});
