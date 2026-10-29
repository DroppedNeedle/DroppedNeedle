import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { apiError, fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';

const state = vi.hoisted(() => ({
	url: new URL('http://localhost/connect'),
	handBack: vi.fn()
}));

vi.mock('$app/state', () => ({
	page: {
		get url() {
			return state.url;
		}
	}
}));

// The real hand-back leaves the page for the app; record it instead.
vi.mock('$lib/utils/appHandBack', async (importOriginal) => {
	const actual = await importOriginal<typeof import('$lib/utils/appHandBack')>();
	// The test page itself runs in a frame.
	return { ...actual, handBack: state.handBack, framed: () => false };
});

import Connect from './+page.svelte';

const QUERY = {
	client_id: 'my-player',
	redirect_uri: 'com.example.player:/connect',
	code_challenge: 'E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM',
	code_challenge_method: 'S256',
	state: 'app state',
	name: 'Pixel 9'
};

const VIEW: RouteData<'GET /api/v3/auth/app-connect/request'> = {
	client_id: 'my-player',
	client_name: 'My Player',
	device_name: 'Pixel 9',
	password_name: 'My Player · Pixel 9',
	replaces_existing: false,
	at_limit: false,
	cancel_redirect: 'com.example.player:/connect?error=access_denied&state=app%20state'
};

function openAt(query: Record<string, string>) {
	state.url = new URL(`http://localhost/connect?${new URLSearchParams(query)}`);
}

function signIn() {
	authStore.setUser({
		id: 'u1',
		display_name: 'Jane Doe',
		role: 'user',
		email: null,
		avatar_url: null,
		username: 'jane',
		username_display: 'Jane',
		providers: ['oidc']
	});
}

afterEach(() => {
	authStore.clear();
	state.handBack.mockReset();
});

describe('app connect page', () => {
	it('asks with the query, shows the app from the server and hands the code back on allow', async () => {
		openAt(QUERY);
		signIn();
		const api = fakeApi({
			'GET /api/v3/auth/app-connect/request': VIEW,
			'POST /api/v3/auth/app-connect/authorize': {
				redirect: 'com.example.player:/connect?code=c0de&state=app%20state'
			}
		});

		await renderWithQuery(Connect, {});

		await expect.element(page.getByRole('heading', { name: 'Connect My Player?' })).toBeVisible();
		await expect.element(page.getByText('My Player · Pixel 9')).toBeVisible();
		await expect.element(page.getByText('Jane', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('Only allow this if you just tapped "Sign in" in this app yourself.'))
			.toBeVisible();
		expect(Object.fromEntries(api.calls('GET /api/v3/auth/app-connect/request')[0]!.query)).toEqual(
			QUERY
		);

		await page.getByRole('button', { name: 'Allow' }).click();

		await expect
			.poll(() => state.handBack.mock.calls[0]?.[0])
			.toBe('com.example.player:/connect?code=c0de&state=app%20state');
		expect(api.calls('POST /api/v3/auth/app-connect/authorize')[0]?.body).toEqual(QUERY);
		await expect.element(page.getByRole('link', { name: 'Open the app' })).toBeVisible();
	});

	it('cancel hands back access_denied without asking the server', async () => {
		openAt(QUERY);
		signIn();
		const api = fakeApi({ 'GET /api/v3/auth/app-connect/request': VIEW });

		await renderWithQuery(Connect, {});
		await page.getByRole('button', { name: 'Cancel' }).click();

		expect(state.handBack).toHaveBeenCalledWith(VIEW.cancel_redirect);
		expect(api.calls('POST /api/v3/auth/app-connect/authorize')).toHaveLength(0);
	});

	it('says when it replaces a password, and refuses at the limit', async () => {
		openAt(QUERY);
		signIn();
		fakeApi({
			'GET /api/v3/auth/app-connect/request': { ...VIEW, replaces_existing: true, at_limit: true }
		});

		await renderWithQuery(Connect, {});

		await expect
			.element(page.getByText('It replaces the app-password of that name you already have.'))
			.toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Allow' })).toBeDisabled();
	});

	it('shows an unknown app as an error and never hands anything back', async () => {
		openAt({ ...QUERY, redirect_uri: 'https://evil.example/' });
		signIn();
		fakeApi({
			'GET /api/v3/auth/app-connect/request': apiError(
				400,
				'INVALID_INPUT',
				'This app is not allowed to sign in to this server.'
			)
		});

		await renderWithQuery(Connect, {});

		await expect
			.element(page.getByText('This app is not allowed to sign in to this server.'))
			.toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Back to the app' }))
			.not.toBeInTheDocument();
		expect(state.handBack).not.toHaveBeenCalled();
	});

	it('offers the way back for a malformed request of a known app', async () => {
		openAt({ ...QUERY, code_challenge_method: 'plain' });
		signIn();
		fakeApi({
			'GET /api/v3/auth/app-connect/request': apiError(
				400,
				'INVALID_INPUT',
				'The app must use the S256 challenge method.',
				{ redirect: 'com.example.player:/connect?error=invalid_request&state=app%20state' }
			)
		});

		await renderWithQuery(Connect, {});

		await expect
			.element(page.getByRole('link', { name: 'Back to the app' }))
			.toHaveAttribute(
				'href',
				'com.example.player:/connect?error=invalid_request&state=app%20state'
			);
	});

	it('says when the feature is off', async () => {
		openAt(QUERY);
		signIn();
		fakeApi({ 'GET /api/v3/auth/app-connect/request': apiError(404, 'NOT_FOUND', 'Not found') });

		await renderWithQuery(Connect, {});

		await expect
			.element(page.getByText('Signing in apps from the browser is not turned on for this server.'))
			.toBeVisible();
	});
});
