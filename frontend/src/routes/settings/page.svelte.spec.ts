import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

vi.mock('$app/state', () => ({
	page: { url: new URL('http://localhost/settings?tab=discover') }
}));
vi.mock('$app/navigation', () => ({
	goto: vi.fn(),
	replaceState: vi.fn(),
	pushState: vi.fn(),
	beforeNavigate: vi.fn(),
	afterNavigate: vi.fn(),
	onNavigate: vi.fn(),
	invalidate: vi.fn(),
	invalidateAll: vi.fn()
}));

import SettingsPage from './+page.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';

const discoverPrefs: RouteData<'GET /api/v3/me/section-prefs'> = {
	pages: {
		discover: [
			{
				key: 'queue',
				title: 'Discover Queue',
				description: 'Albums dealt to you one at a time.',
				zone: 'Discover',
				enabled: true,
				available: true
			}
		]
	}
};

const upToDate: RouteData<'GET /api/v3/version/check-update'> = {
	comparison_failed: false,
	current_version: '1.0.0',
	update_available: false
};

beforeEach(() => {
	localStorage.clear();
	authStore.setUser(
		toAuthUser({
			id: 'user-1',
			display_name: 'Listener',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'listener',
			username_display: 'Listener'
		})
	);
});

describe('settings for a member', () => {
	it('opens the Discover section settings that the Customise buttons link to', async () => {
		fakeApi({
			'GET /api/v3/me/section-prefs': discoverPrefs,
			'GET /api/v3/version/check-update': upToDate
		});
		await renderWithQuery(SettingsPage, {});

		await expect
			.element(page.getByRole('button', { name: 'Discover', exact: true }))
			.toHaveAttribute('aria-current', 'page');
		await expect.element(page.getByRole('button', { name: 'Home', exact: true })).toBeVisible();
		await expect.element(page.getByText('Discover Queue')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Library', exact: true }))
			.not.toBeInTheDocument();
	});
});
