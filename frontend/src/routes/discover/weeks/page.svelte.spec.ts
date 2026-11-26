import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { dayMonth } from '$lib/features/discover/state/weekWords';
import WeeksPage from './+page.svelte';

const WEEKS = 'GET /api/v3/discover/weeks';
const LATER = 'GET /api/v3/discover/later';
type SoFar = NonNullable<RouteData<typeof WEEKS>['record']>;

const COUNTED_FROM = Date.UTC(2026, 11, 20);

function record(more: Partial<SoFar>): SoFar {
	return {
		picked: 24,
		sampled: 17,
		answers: { kept: 9, later: 3, known: 4, passed: 3, open: 5 },
		counted_from_at: COUNTED_FROM,
		month_on: null,
		still_played: null,
		...more
	};
}

async function show(shown: SoFar) {
	fakeApi({
		[WEEKS]: { items: [], next_cursor: null, total: null, record: shown },
		[LATER]: { items: [], next_cursor: null, total: 0 }
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
	await renderWithQuery(WeeksPage, {});
}

describe('the Past weeks page', () => {
	it('says from when "still played" counts until a kept album has had its month', async () => {
		await show(record({}));

		const so = page.getByRole('region', { name: 'What your sets came to' });
		await expect.element(so.getByText('Sampled')).toBeVisible();
		await expect.element(so.getByText('17')).toBeVisible();
		await expect
			.element(so.getByText(`Still played a month on: counted from ${dayMonth(COUNTED_FROM)}`))
			.toBeVisible();
		await expect.element(so.getByText('Kept a month on')).not.toBeInTheDocument();
		await expect.element(so.getByText('Still played', { exact: true })).not.toBeInTheDocument();
	});

	it('counts the kept albums a month on and the ones still played', async () => {
		await show(record({ month_on: 9, still_played: 6 }));

		const so = page.getByRole('region', { name: 'What your sets came to' });
		const month = so.getByRole('definition').filter({ hasText: '6' });
		await expect.element(month).toBeVisible();
		await expect.element(so.getByText('Kept a month on')).toBeVisible();
		await expect.element(so.getByText('Still played', { exact: true })).toBeVisible();
		await expect.element(so.getByText('6 of 9 kept albums still played a month on.')).toBeVisible();
		await expect.element(so.getByText(/counted from/)).not.toBeInTheDocument();
	});
});
