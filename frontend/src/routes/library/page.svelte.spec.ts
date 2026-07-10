import { page } from '@vitest/browser/context';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import { authStore } from '$lib/stores/authStore.svelte';
import type { AuthUser } from '$lib/stores/authStore.svelte';

// The dashboard pulls in queries / SSE / stores; stub it so this test covers the
// route shell (header + action buttons) in isolation.
vi.mock('$lib/components/library/LibraryDashboard.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});

import LibraryPage from './+page.svelte';

function user(role: AuthUser['role']): AuthUser {
	return {
		id: 'u-1',
		display_name: 'Test',
		role,
		email: null,
		avatar_url: null,
		username: 'test',
		username_display: 'test',
		providers: ['local']
	};
}

afterEach(() => authStore.clear());

describe('library route page', () => {
	it('renders the Library header and subtitle', async () => {
		render(LibraryPage);
		await expect.element(page.getByRole('heading', { name: 'Vinyl Collection' })).toBeVisible();
		await expect
			.element(page.getByText('Everything you own, curated in your vault.'))
			.toBeVisible();
	});

	it('links Listen to the Listening Room', async () => {
		render(LibraryPage);
		await expect
			.element(page.getByRole('link', { name: 'Listen' }))
			.toHaveAttribute('href', '/library/local');
	});

	it('points a non-admin to their own Profile for Connect Apps', async () => {
		authStore.setUser(user('user'));
		render(LibraryPage);
		await expect
			.element(page.getByRole('link', { name: 'Connect Apps' }))
			.toHaveAttribute('href', '/profile#connect-apps');
	});

	it('points an admin to the server-setup tab for Connect Apps', async () => {
		authStore.setUser(user('admin'));
		render(LibraryPage);
		await expect
			.element(page.getByRole('link', { name: 'Connect Apps' }))
			.toHaveAttribute('href', '/settings?tab=connect-apps');
	});
});
