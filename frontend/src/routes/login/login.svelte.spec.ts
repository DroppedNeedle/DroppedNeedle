import { page, userEvent } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';

const mockLocalMutate = vi.fn().mockResolvedValue({
	user: {
		id: 'u1',
		display_name: 'Jane',
		role: 'user',
		email: null,
		avatar_url: null,
		username: 'jane.doe',
		username_display: 'Jane.Doe'
	}
});
const mockSetUser = vi.fn();
const mockGoto = vi.fn();

vi.mock('$app/navigation', () => ({ goto: (...args: unknown[]) => mockGoto(...args) }));

vi.mock('$lib/api/client', () => ({
	api: { global: { get: vi.fn() } },
	ApiError: class ApiError extends Error {}
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { setUser: (...args: unknown[]) => mockSetUser(...args) }
}));

// Two providers so the tab bar renders (it only shows when >1 method is enabled).
vi.mock('$lib/queries/auth/AuthProvidersQuery.svelte', () => ({
	getAuthProvidersQuery: () => ({
		data: { local: true, plex: false, jellyfin: true, oidc: false },
		isSuccess: true
	})
}));

vi.mock('$lib/queries/auth/AuthMutations.svelte', () => ({
	createLocalLoginMutation: () => ({ mutateAsync: mockLocalMutate, isPending: false }),
	createJellyfinLoginMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createOidcAuthorizeMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createPlexPinMutation: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

import Login from './+page.svelte';

beforeEach(() => {
	vi.clearAllMocks();
});

describe('login local tab uses a username field', () => {
	it('keeps the auth-only theme and component styles active', async () => {
		await render(Login);
		const heading = page.getByRole('heading', { name: 'DroppedNeedle' });
		const submit = page.getByRole('button', { name: 'Sign in' });
		await expect.element(heading).toBeVisible();
		await expect.element(submit).toBeVisible();
		const wrapper = heading.element().parentElement?.parentElement?.parentElement;
		expect(wrapper).toBeInstanceOf(HTMLElement);
		expect(getComputedStyle(heading.element()).fontFamily).toContain('Space Grotesk');
		expect(getComputedStyle(wrapper as HTMLElement).backgroundImage).not.toBe('none');
		expect(getComputedStyle(submit.element()).backgroundColor).not.toBe('rgba(0, 0, 0, 0)');
	});

	it('renders a text username input (autocomplete=username), not an email input', async () => {
		await render(Login);
		const input = page.getByPlaceholder('Username');
		await expect.element(input).toBeInTheDocument();
		const el = input.element() as HTMLInputElement;
		expect(el.type).toBe('text');
		expect(el.getAttribute('autocomplete')).toBe('username');
		// The local tab no longer collects an email address.
		expect(document.querySelector('input[type="email"]')).toBeNull();
	});

	it('labels the local tab "Username"', async () => {
		await render(Login);
		await expect.element(page.getByRole('button', { name: 'Username' })).toBeInTheDocument();
	});

	it('offers local-account recovery from the password field', async () => {
		await render(Login);
		await expect
			.element(page.getByRole('link', { name: 'Forgot password?' }))
			.toHaveAttribute('href', '/recover-password');
	});

	it('submits the mixed-case username + password to the local login mutation', async () => {
		await render(Login);
		await userEvent.fill(page.getByPlaceholder('Username'), 'Jane.Doe');
		await userEvent.fill(page.getByPlaceholder('Password'), 'a-strong-password');
		await page.getByRole('button', { name: 'Sign in' }).click();
		expect(mockLocalMutate).toHaveBeenCalledWith({
			username: 'Jane.Doe',
			password: 'a-strong-password'
		});
	});
});
