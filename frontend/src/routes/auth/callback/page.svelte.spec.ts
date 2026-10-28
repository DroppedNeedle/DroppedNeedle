import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';

vi.mock('$app/state', () => ({
	page: { url: new URL('http://localhost/auth/callback?code=one-time-code') }
}));

// The page leaves for the home page with a full load once signed in; send
// that to a fragment so the test page stays put.
vi.mock('$lib/utils/basePath', async (importOriginal) => {
	const actual = await importOriginal<typeof import('$lib/utils/basePath')>();
	return {
		...actual,
		withBasePath: (path: string) => (path === '/' ? '#signed-in' : actual.withBasePath(path))
	};
});

import Callback from './+page.svelte';

const session: RouteData<'POST /api/v3/auth/oidc/exchange'> = {
	user: {
		id: 'u1',
		username: 'jane.doe',
		username_display: 'Jane.Doe',
		display_name: 'Jane Doe',
		email: 'jane.doe@example.com',
		avatar_url: null,
		role: 'admin',
		providers: ['oidc'],
		created_at: 1_700_000_000,
		last_login_at: 1_700_000_000
	}
};

afterEach(() => {
	authStore.clear();
	history.replaceState(null, '', window.location.pathname + window.location.search);
});

describe('OIDC callback', () => {
	it('signs in with the exchanged session and leaves for the home page', async () => {
		const api = fakeApi({ 'POST /api/v3/auth/oidc/exchange': session });

		await renderWithQuery(Callback, {});

		await expect.poll(() => window.location.hash).toBe('#signed-in');
		expect(authStore.user).toMatchObject({ id: 'u1', role: 'admin', providers: ['oidc'] });
		expect(api.calls('POST /api/v3/auth/oidc/exchange')[0]?.body).toEqual({
			code: 'one-time-code'
		});
		await expect.element(page.getByText('Could not reach the server.')).not.toBeInTheDocument();
	});
});
