import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const state = vi.hoisted(() => ({
	url: new URL('http://localhost/login'),
	providers: { local: true, plex: false, jellyfin: false, oidc: true, oidc_auto_redirect: true }
}));
// An address the page refuses to follow, so the test page never navigates.
const mockAuthorize = vi.fn().mockResolvedValue({ authorize_url: 'javascript:void(0)' });

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
vi.mock('$app/state', () => ({
	page: {
		get url() {
			return state.url;
		}
	}
}));
vi.mock('$lib/api/client', () => ({ api: { get: vi.fn() } }));
vi.mock('$lib/stores/authStore.svelte', () => ({ authStore: { setUser: vi.fn() } }));
vi.mock('$lib/features/auth/queries/AuthProvidersQuery.svelte', () => ({
	getAuthProvidersQuery: () => ({ data: state.providers, isSuccess: true })
}));
vi.mock('$lib/features/auth/queries/AuthMutations.svelte', () => ({
	createLocalLoginMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createJellyfinLoginMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createOidcAuthorizeMutation: () => ({ mutateAsync: mockAuthorize, isPending: false })
}));
vi.mock('$lib/features/settings/queries/plex.svelte', () => ({
	createPlexStartMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	pollPlexFlow: vi.fn()
}));

import Login from './+page.svelte';

beforeEach(() => {
	mockAuthorize.mockClear();
	state.url = new URL('http://localhost/login');
	state.providers = {
		local: true,
		plex: false,
		jellyfin: false,
		oidc: true,
		oidc_auto_redirect: true
	};
});

describe('login page going straight to single sign-on', () => {
	it('starts single sign-on on load when the admin asked for it', async () => {
		await render(Login);
		await expect.poll(() => mockAuthorize.mock.calls.length).toBe(1);
		await expect.element(page.getByText('Sign-in provider address refused')).toBeVisible();
	});

	it('shows the sign-in methods with ?local', async () => {
		state.url = new URL('http://localhost/login?local');
		await render(Login);
		await expect.element(page.getByPlaceholder('Username')).toBeVisible();
		expect(mockAuthorize).not.toHaveBeenCalled();
	});

	it('waits for a click while the setting is off', async () => {
		state.providers = { ...state.providers, oidc_auto_redirect: false };
		await render(Login);
		await expect.element(page.getByPlaceholder('Username')).toBeVisible();
		expect(mockAuthorize).not.toHaveBeenCalled();
	});
});
