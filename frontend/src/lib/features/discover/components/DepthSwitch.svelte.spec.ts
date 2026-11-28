import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import DepthSwitch from './DepthSwitch.svelte';

const PREFS = 'GET /api/v3/discover/prefs';
const SAVE = 'PUT /api/v3/discover/prefs';
type Prefs = RouteData<typeof PREFS>;

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

describe('DepthSwitch.svelte', () => {
	it('shows the stored depth, and a choice saves it with the set day kept', async () => {
		let prefs: Prefs = { set_day: 'monday', depth: 'home_ground', app_playlists: true };
		const api = fakeApi({
			[PREFS]: () => prefs,
			[SAVE]: ({ body }) => {
				prefs = body as Prefs;
				return prefs;
			}
		});
		signIn();
		// The mutation's effects sweep the app's own client.
		await renderWithQuery(DepthSwitch, {}, { client: queryClient });

		await expect.element(page.getByRole('radio', { name: 'Home ground' })).toBeChecked();
		await expect.element(page.getByText('close to what you play.')).toBeVisible();
		await expect.element(page.getByText('Your next set will follow this.')).not.toBeInTheDocument();

		await page.getByRole('radio', { name: 'Far out' }).click();
		await expect.element(page.getByRole('radio', { name: 'Far out' })).toBeChecked();
		await expect.element(page.getByText('further from what you know.')).toBeVisible();
		await expect
			.element(page.getByText('Your next set will follow this. This week holds still.'))
			.toBeVisible();
		expect(api.calls(SAVE).map((call) => call.body)).toEqual([
			{ set_day: 'monday', depth: 'far_out', app_playlists: true }
		]);
	});
});
