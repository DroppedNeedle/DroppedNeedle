import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';

import DiscoveryAlbumCarousel from './DiscoveryAlbumCarousel.svelte';

function signIn(role: 'admin' | 'user') {
	authStore.setUser(
		toAuthUser({
			id: `${role}-1`,
			display_name: role,
			role,
			email: null,
			avatar_url: null,
			username: role,
			username_display: role
		})
	);
}

const unconfigured = { albums: [], configured: false, title: 'You Might Also Like' };

afterEach(() => authStore.clear());

describe('DiscoveryAlbumCarousel', () => {
	it('says recommendations are off, with Settings for an admin', async () => {
		signIn('admin');
		await render(DiscoveryAlbumCarousel, unconfigured);
		await expect.element(page.getByText('Recommendations are turned off.')).toBeVisible();
		await expect.element(page.getByRole('link', { name: 'Settings' })).toBeVisible();
	});

	it('gives anyone else no Settings link', async () => {
		signIn('user');
		await render(DiscoveryAlbumCarousel, unconfigured);
		await expect.element(page.getByText('Recommendations are turned off.')).toBeVisible();
		await expect.element(page.getByRole('link', { name: 'Settings' })).not.toBeInTheDocument();
	});
});
