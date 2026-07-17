import { describe, expect, it, vi, beforeEach } from 'vitest';
import { AuthQueryKeyFactory } from './AuthQueryKeyFactory';
import { AUTH_ENDPOINTS } from './endpoints';
import { toAuthUser } from './types';

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory()),
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: {
		post: vi.fn(),
		global: {
			get: vi.fn(),
			post: vi.fn()
		}
	}
}));

import { api } from '$lib/api/client';
import { createMutation, createQuery } from '@tanstack/svelte-query';

const mockGet = vi.mocked(api.global.get);
const mockPost = vi.mocked(api.global.post);
const mockAuthenticatedPost = vi.mocked(api.post);
const mockCreateMutation = vi.mocked(createMutation);
const mockCreateQuery = vi.mocked(createQuery);

beforeEach(() => {
	vi.clearAllMocks();
});

/** Pulls the mutationFn out of the most recently created mutation. */
function lastMutationFn(): (vars: unknown) => Promise<unknown> {
	const factory = mockCreateMutation.mock.calls[
		mockCreateMutation.mock.calls.length - 1
	][0] as unknown as () => { mutationFn: (vars: unknown) => Promise<unknown> };
	return factory().mutationFn;
}

describe('AuthQueryKeyFactory', () => {
	it('providers key is [auth, providers]', () => {
		expect(AuthQueryKeyFactory.providers()).toEqual(['auth', 'providers']);
	});
});

describe('toAuthUser', () => {
	it('maps a session user (incl. username fields) onto the auth store shape', () => {
		const user = toAuthUser({
			id: 'u1',
			display_name: 'Alice',
			role: 'admin',
			email: 'a@example.com',
			avatar_url: null,
			username: 'alice',
			username_display: 'Alice'
		});
		expect(user).toEqual({
			id: 'u1',
			display_name: 'Alice',
			role: 'admin',
			email: 'a@example.com',
			avatar_url: null,
			username: 'alice',
			username_display: 'Alice',
			providers: []
		});
	});
});

describe('auth mutations route through api.global', () => {
	it('local login posts username + password to the login endpoint', async () => {
		mockPost.mockResolvedValue({ user: {} });
		const { createLocalLoginMutation } = await import('./AuthMutations.svelte');
		createLocalLoginMutation();

		await lastMutationFn()({ username: 'jane.doe', password: 'pw' });

		expect(mockPost).toHaveBeenCalledWith(AUTH_ENDPOINTS.login, {
			username: 'jane.doe',
			password: 'pw'
		});
	});

	it('setup posts username and omits email when not provided', async () => {
		mockPost.mockResolvedValue({ user: {} });
		const { createSetupMutation } = await import('./AuthMutations.svelte');
		createSetupMutation();

		await lastMutationFn()({ display_name: 'A', username: 'a.admin', password: 'pw' });

		expect(mockPost).toHaveBeenCalledWith(AUTH_ENDPOINTS.setup, {
			display_name: 'A',
			username: 'a.admin',
			password: 'pw'
		});
	});

	it('oidc exchange posts the authorization code', async () => {
		mockPost.mockResolvedValue({ user: {} });
		const { createOidcExchangeMutation } = await import('./AuthMutations.svelte');
		createOidcExchangeMutation();

		await lastMutationFn()({ code: 'abc' });

		expect(mockPost).toHaveBeenCalledWith(AUTH_ENDPOINTS.oidcExchange, { code: 'abc' });
	});

	it('plex pin posts to the pin endpoint with no body', async () => {
		mockPost.mockResolvedValue({ pin_id: 'p', auth_url: 'u' });
		const { createPlexPinMutation } = await import('./AuthMutations.svelte');
		createPlexPinMutation();

		await lastMutationFn()(undefined);

		expect(mockPost).toHaveBeenCalledWith(AUTH_ENDPOINTS.plexPin);
	});

	it('password recovery posts the code and new password without a session', async () => {
		mockPost.mockResolvedValue(undefined);
		const { createPasswordRecoveryResetMutation } = await import('./AuthMutations.svelte');
		createPasswordRecoveryResetMutation();

		await lastMutationFn()({
			username: 'alice',
			recovery_code: 'AAAA-BBBB-CCCC-DDDD-EEEE',
			new_password: 'a new secure password'
		});

		expect(mockPost).toHaveBeenCalledWith('/api/v1/auth/password-recovery/reset', {
			username: 'alice',
			recovery_code: 'AAAA-BBBB-CCCC-DDDD-EEEE',
			new_password: 'a new secure password'
		});
	});

	it('admin recovery-code generation uses the authenticated client', async () => {
		mockAuthenticatedPost.mockResolvedValue({
			recovery_code: 'AAAA-BBBB-CCCC-DDDD-EEEE',
			expires_at: '2026-07-17T17:00:00Z'
		});
		const { createPasswordRecoveryCodeMutation } = await import('./AuthMutations.svelte');
		createPasswordRecoveryCodeMutation();

		await lastMutationFn()('user/1');

		expect(mockAuthenticatedPost).toHaveBeenCalledWith(
			'/api/v1/auth/admin/users/user%2F1/password-recovery'
		);
	});
});

describe('AuthProvidersQuery', () => {
	it('fetches the providers endpoint via api.global.get', async () => {
		mockGet.mockResolvedValue({ local: true, plex: false, jellyfin: false, oidc: false });
		const { getAuthProvidersQuery } = await import('./AuthProvidersQuery.svelte');
		getAuthProvidersQuery();

		const factory = mockCreateQuery.mock.calls[
			mockCreateQuery.mock.calls.length - 1
		][0] as unknown as () => Record<string, unknown>;
		const opts = factory();
		const queryFn = opts.queryFn as (ctx: { signal: AbortSignal }) => Promise<unknown>;
		await queryFn({ signal: new AbortController().signal });

		expect(mockGet).toHaveBeenCalledTimes(1);
		expect(mockGet.mock.calls[0][0]).toBe(AUTH_ENDPOINTS.providers);
	});
});
