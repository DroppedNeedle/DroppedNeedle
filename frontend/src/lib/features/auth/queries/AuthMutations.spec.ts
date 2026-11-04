import { v3 } from '$lib/api/v3/endpoint';
import { describe, expect, it, vi, beforeEach, type Mock } from 'vitest';
import { toAuthUser } from './types';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/queries/effects', () => ({
	createAppMutation: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { v3: { GET: vi.fn(), POST: vi.fn() } }
}));

import { api } from '$lib/api/client';
import { createQuery } from '@tanstack/svelte-query';
import { createAppMutation } from '$lib/queries/effects';
import {
	createLocalLoginMutation,
	createOidcExchangeMutation,
	createPasswordRecoveryCodeMutation,
	createPasswordRecoveryResetMutation,
	createSetupMutation
} from './AuthMutations.svelte';
import { getAuthProvidersQuery } from './AuthProvidersQuery.svelte';

const mockV3Get = vi.mocked(api.v3.GET) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockV3Post = vi.mocked(api.v3.POST) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockCreateMutation = vi.mocked(createAppMutation);
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

describe('auth calls', () => {
	it('send each sign-in, setup and recovery call to its endpoint', async () => {
		const cases: [() => unknown, unknown, unknown[]][] = [
			[
				createLocalLoginMutation,
				{ username: 'jane.doe', password: 'pw' },
				[v3('/api/v3/auth/login'), { username: 'jane.doe', password: 'pw' }]
			],
			// Setup leaves the email out when none is given.
			[
				createSetupMutation,
				{ display_name: 'A', username: 'a.admin', password: 'pw' },
				[v3('/api/v3/auth/setup'), { display_name: 'A', username: 'a.admin', password: 'pw' }]
			],
			[
				createOidcExchangeMutation,
				{ code: 'abc' },
				[v3('/api/v3/auth/oidc/exchange'), { code: 'abc' }]
			],
			// Recovery posts the code and new password without a session.
			[
				createPasswordRecoveryResetMutation,
				{
					username: 'alice',
					recovery_code: 'AAAA-BBBB-CCCC-DDDD-EEEE',
					new_password: 'a new secure password'
				},
				[
					'/api/v3/auth/password-recovery/reset',
					{
						username: 'alice',
						recovery_code: 'AAAA-BBBB-CCCC-DDDD-EEEE',
						new_password: 'a new secure password'
					}
				]
			],
			// An admin makes a recovery code through the signed-in client.
			[createPasswordRecoveryCodeMutation, 'user/1', ['/api/v3/admin/users/user%2F1/recovery-code']]
		];
		for (const [create, input, expected] of cases) {
			mockV3Post.mockReset();
			mockV3Post.mockResolvedValue({ user: {} });
			create();

			await lastMutationFn()(input);

			expect(mockV3Post).toHaveBeenCalledWith(...expected);
		}

		mockV3Get.mockResolvedValue({ local: true, plex: false, jellyfin: false, oidc: false });
		getAuthProvidersQuery();
		const factory = mockCreateQuery.mock.calls[
			mockCreateQuery.mock.calls.length - 1
		][0] as unknown as () => Record<string, unknown>;
		const queryFn = factory().queryFn as (ctx: { signal: AbortSignal }) => Promise<unknown>;
		await queryFn({ signal: new AbortController().signal });

		expect(mockV3Get).toHaveBeenCalledTimes(1);
		expect(mockV3Get.mock.calls[0][0]).toBe(v3('/api/v3/auth/providers'));
	});
});
