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
		await expect.element(page.getByRole('heading', { name: 'Library' })).toBeVisible();
		await expect.element(page.getByText('Your scanned music library')).toBeVisible();
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

	it('points an admin to their Profile for Connect Apps', async () => {
		authStore.setUser(user('admin'));
		render(LibraryPage);
		await expect
			.element(page.getByRole('link', { name: 'Connect Apps' }))
			.toHaveAttribute('href', '/profile#connect-apps');
	});

	it('links Controls to Library Management for administrators', async () => {
		authStore.setUser(user('admin'));
		render(LibraryPage);
		await expect
			.element(page.getByRole('link', { name: 'Controls' }))
			.toHaveAttribute('href', '/library/management');
	});

	it('keeps Controls visible but locked for non-administrators', async () => {
		authStore.setUser(user('user'));
		render(LibraryPage);
		const controls = page.getByRole('button', { name: 'Controls' });
		await expect.element(controls).toBeDisabled();
		await expect.element(controls).toHaveAttribute('title', 'Administrator access required');
		await expect.element(page.getByRole('link', { name: 'Controls' })).not.toBeInTheDocument();
	});
});
