import { beforeEach, describe, expect, it, vi } from 'vitest';

import { appPasswordsChanged, connectAppsSettingsChanged, type Effect } from '$lib/queries/effects';

vi.mock('$lib/queries/effects', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/effects')>()),
	createAppMutation: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { v3: { GET: vi.fn(), POST: vi.fn(), PUT: vi.fn(), DELETE: vi.fn() } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-1' } }
}));

import { api } from '$lib/api/client';
import { createAppMutation } from '$lib/queries/effects';

const mockPut = vi.mocked(api.v3.PUT) as unknown as ReturnType<typeof vi.fn>;
const mockPost = vi.mocked(api.v3.POST) as unknown as ReturnType<typeof vi.fn>;
const mockDelete = vi.mocked(api.v3.DELETE) as unknown as ReturnType<typeof vi.fn>;
const mockCreateMutation = vi.mocked(createAppMutation);

beforeEach(() => vi.clearAllMocks());

function lastMutationOpts(): Record<string, unknown> {
	const calls = mockCreateMutation.mock.calls;
	return (calls[calls.length - 1][0] as unknown as () => Record<string, unknown>)();
}

describe('saveConnectAppsSettings', () => {
	it('PUTs the settings and declares them changed', async () => {
		mockPut.mockResolvedValue({});
		const { saveConnectAppsSettings } = await import('./ConnectAppsMutations.svelte');
		saveConnectAppsSettings();
		const opts = lastMutationOpts();
		await (opts.mutationFn as (v: unknown) => Promise<unknown>)({ subsonic_enabled: true });
		expect(mockPut).toHaveBeenCalledWith('/api/v3/settings/connect-apps', {
			subsonic_enabled: true
		});
		expect((opts.effects as () => Effect[])()).toEqual([connectAppsSettingsChanged()]);
	});
});

describe('createAppPassword', () => {
	it("POSTs {name} and declares the caller's app passwords changed", async () => {
		mockPost.mockResolvedValue({ secret: 's', app_password: {} });
		const { createAppPassword } = await import('./ConnectAppsMutations.svelte');
		createAppPassword();
		const opts = lastMutationOpts();
		await (opts.mutationFn as (v: string) => Promise<unknown>)('Symfonium (phone)');
		expect(mockPost).toHaveBeenCalledWith('/api/v3/me/app-passwords', {
			name: 'Symfonium (phone)'
		});
		expect((opts.effects as () => Effect[])()).toEqual([appPasswordsChanged('user-1')]);
	});
});

describe('revokeAppPassword', () => {
	it("DELETEs by id and declares the caller's app passwords changed", async () => {
		mockDelete.mockResolvedValue(undefined);
		const { revokeAppPassword } = await import('./ConnectAppsMutations.svelte');
		revokeAppPassword();
		const opts = lastMutationOpts();
		await (opts.mutationFn as (v: string) => Promise<unknown>)('ap-1');
		expect(mockDelete).toHaveBeenCalledWith('/api/v3/me/app-passwords/ap-1');
		expect((opts.effects as () => Effect[])()).toEqual([appPasswordsChanged('user-1')]);
	});
});

describe('adminRevokeAppPassword', () => {
	type Revoke = { id: string; userId: string };
	const declared = (opts: Record<string, unknown>, vars: Revoke) =>
		(opts.effects as (d: unknown, v: Revoke) => Effect[])(undefined, vars).map((e) => e.name);

	it('DELETEs the admin endpoint and refreshes the roster only (other user)', async () => {
		mockDelete.mockResolvedValue(undefined);
		const { adminRevokeAppPassword } = await import('./ConnectAppsMutations.svelte');
		adminRevokeAppPassword();
		const opts = lastMutationOpts();
		await (opts.mutationFn as (v: Revoke) => Promise<unknown>)({
			id: 'ap-9',
			userId: 'user-bob'
		});
		expect(mockDelete).toHaveBeenCalledWith('/api/v3/admin/app-passwords/ap-9');
		// another user's revoke must NOT touch the admin's own per-user key
		expect(declared(opts, { id: 'ap-9', userId: 'user-bob' })).toEqual([
			'appPasswordRosterChanged'
		]);
	});

	it("also refreshes the admin's own list when they revoke their own password", async () => {
		const { adminRevokeAppPassword } = await import('./ConnectAppsMutations.svelte');
		adminRevokeAppPassword();
		const opts = lastMutationOpts();
		expect(declared(opts, { id: 'ap-self', userId: 'user-1' })).toEqual([
			'appPasswordRosterChanged',
			'appPasswordsChanged'
		]);
	});
});
