import { page } from 'vitest/browser';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

vi.mock('$app/state', async () => ({ page: (await import('$lib/test/page.svelte')).testPage }));
vi.mock('$app/navigation', async () => ({
	goto: (await import('$lib/test/page.svelte')).testGoto
}));

import Login from './+page.svelte';
import { testPage } from '$lib/test/page.svelte';
import { SSO_AUTO_START_KEY } from '$lib/utils/ssoAutoStart';

const straightToSso: RouteData<'GET /api/v3/auth/providers'> = {
	local: true,
	plex: false,
	jellyfin: false,
	oidc: true,
	oidc_auto_redirect: true
};

// An address the page refuses to follow, so the test page never navigates.
const refusedStart: RouteData<'POST /api/v3/auth/oidc/authorize'> = {
	authorize_url: 'javascript:void(0)'
};

// The SSO tab shows once the providers answer is in; a start would have
// switched the page to that tab, so the username field would be gone.
async function expectMethodsAfterProvidersLoad() {
	await expect.element(page.getByRole('button', { name: 'SSO', exact: true })).toBeVisible();
	await expect.element(page.getByPlaceholder('Username')).toBeVisible();
}

beforeEach(() => {
	testPage.url = new URL('http://localhost/login');
});

afterEach(() => {
	sessionStorage.removeItem(SSO_AUTO_START_KEY);
});

describe('login page going straight to single sign-on', () => {
	it('starts single sign-on once on load when the admin asked for it', async () => {
		const api = fakeApi({
			'GET /api/v3/auth/providers': straightToSso,
			'POST /api/v3/auth/oidc/authorize': refusedStart
		});

		await renderWithQuery(Login, {});

		await expect.element(page.getByText('Sign-in provider address refused')).toBeVisible();
		expect(api.calls('POST /api/v3/auth/oidc/authorize')).toHaveLength(1);
	});

	it('shows the sign-in methods when this tab started it under a minute ago', async () => {
		// A sign-in that came straight back to the login page: starting again would loop.
		sessionStorage.setItem(SSO_AUTO_START_KEY, String(Date.now() - 5_000));
		const api = fakeApi({
			'GET /api/v3/auth/providers': straightToSso,
			'POST /api/v3/auth/oidc/authorize': refusedStart
		});

		await renderWithQuery(Login, {});

		await expectMethodsAfterProvidersLoad();
		expect(api.calls('POST /api/v3/auth/oidc/authorize')).toHaveLength(0);
	});

	it('shows the sign-in methods with ?local', async () => {
		testPage.url = new URL('http://localhost/login?local');
		const api = fakeApi({
			'GET /api/v3/auth/providers': straightToSso,
			'POST /api/v3/auth/oidc/authorize': refusedStart
		});

		await renderWithQuery(Login, {});

		await expectMethodsAfterProvidersLoad();
		expect(api.calls('POST /api/v3/auth/oidc/authorize')).toHaveLength(0);
	});

	it('waits for a click while the setting is off', async () => {
		const api = fakeApi({
			'GET /api/v3/auth/providers': { ...straightToSso, oidc_auto_redirect: false },
			'POST /api/v3/auth/oidc/authorize': refusedStart
		});

		await renderWithQuery(Login, {});

		await expectMethodsAfterProvidersLoad();
		expect(api.calls('POST /api/v3/auth/oidc/authorize')).toHaveLength(0);
	});
});
