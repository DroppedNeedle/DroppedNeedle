import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ConnectAppsQueryKeyFactory } from '$lib/queries/featureKeys';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn(async () => undefined),
	setQueryDataWithPersister: vi.fn()
}));

vi.mock('$lib/api/client', () => ({
	api: { v3: { GET: vi.fn(), POST: vi.fn(), PUT: vi.fn(), DELETE: vi.fn() } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-1' } }
}));

import { api } from '$lib/api/client';
import { createMutation } from '@tanstack/svelte-query';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';

const mockPut = vi.mocked(api.v3.PUT) as unknown as ReturnType<typeof vi.fn>;
const mockPost = vi.mocked(api.v3.POST) as unknown as ReturnType<typeof vi.fn>;
const mockDelete = vi.mocked(api.v3.DELETE) as unknown as ReturnType<typeof vi.fn>;
const mockCreateMutation = vi.mocked(createMutation);

beforeEach(() => vi.clearAllMocks());

function lastMutationOpts(): Record<string, unknown> {
	const calls = mockCreateMutation.mock.calls;
	return (calls[calls.length - 1][0] as unknown as () => Record<string, unknown>)();
}

/** The cache keys a success refreshes, in order. */
async function refreshedBy(opts: Record<string, unknown>, vars?: unknown): Promise<unknown[]> {
	await (opts.onSuccess as (d: unknown, v: unknown) => Promise<void>)(undefined, vars);
	return vi
		.mocked(invalidateQueriesWithPersister)
		.mock.calls.map(([filters]) => (filters as { queryKey: unknown }).queryKey);
}

describe('createSaveConnectAppsSettingsMutation', () => {
	it('PUTs the settings and declares them changed', async () => {
		mockPut.mockResolvedValue({});
		const { createSaveConnectAppsSettingsMutation } =
			await import('$lib/features/profile/queries/connect-apps.svelte');
		createSaveConnectAppsSettingsMutation();
		const opts = lastMutationOpts();
		await (opts.mutationFn as (v: unknown) => Promise<unknown>)({ subsonic_enabled: true });
		expect(mockPut).toHaveBeenCalledWith('/api/v3/settings/connect-apps', {
			subsonic_enabled: true
		});
		expect(await refreshedBy(opts)).toEqual([ConnectAppsQueryKeyFactory.settings()]);
	});
});

describe('createAppPasswordMutation', () => {
	it("POSTs {name} and declares the caller's app passwords changed", async () => {
		mockPost.mockResolvedValue({ secret: 's', app_password: {} });
		const { createAppPasswordMutation } =
			await import('$lib/features/profile/queries/connect-apps.svelte');
		createAppPasswordMutation();
		const opts = lastMutationOpts();
		await (opts.mutationFn as (v: string) => Promise<unknown>)('Symfonium (phone)');
		expect(mockPost).toHaveBeenCalledWith('/api/v3/me/app-passwords', {
			name: 'Symfonium (phone)'
		});
		expect(await refreshedBy(opts)).toEqual([ConnectAppsQueryKeyFactory.appPasswords('user-1')]);
	});
});

describe('createRevokeAppPasswordMutation', () => {
	it("DELETEs by id and declares the caller's app passwords changed", async () => {
		mockDelete.mockResolvedValue(undefined);
		const { createRevokeAppPasswordMutation } =
			await import('$lib/features/profile/queries/connect-apps.svelte');
		createRevokeAppPasswordMutation();
		const opts = lastMutationOpts();
		await (opts.mutationFn as (v: string) => Promise<unknown>)('ap-1');
		expect(mockDelete).toHaveBeenCalledWith('/api/v3/me/app-passwords/ap-1');
		expect(await refreshedBy(opts)).toEqual([ConnectAppsQueryKeyFactory.appPasswords('user-1')]);
	});
});

describe('createAdminRevokeAppPasswordMutation', () => {
	type Revoke = { id: string; userId: string };
	it('DELETEs the admin endpoint and refreshes the roster only (other user)', async () => {
		mockDelete.mockResolvedValue(undefined);
		const { createAdminRevokeAppPasswordMutation } =
			await import('$lib/features/profile/queries/connect-apps.svelte');
		createAdminRevokeAppPasswordMutation();
		const opts = lastMutationOpts();
		await (opts.mutationFn as (v: Revoke) => Promise<unknown>)({
			id: 'ap-9',
			userId: 'user-bob'
		});
		expect(mockDelete).toHaveBeenCalledWith('/api/v3/admin/app-passwords/ap-9');
		// another user's revoke must NOT touch the admin's own per-user key
		expect(await refreshedBy(opts, { id: 'ap-9', userId: 'user-bob' })).toEqual([
			ConnectAppsQueryKeyFactory.adminAppPasswords()
		]);
	});

	it("also refreshes the admin's own list when they revoke their own password", async () => {
		const { createAdminRevokeAppPasswordMutation } =
			await import('$lib/features/profile/queries/connect-apps.svelte');
		createAdminRevokeAppPasswordMutation();
		const opts = lastMutationOpts();
		expect(await refreshedBy(opts, { id: 'ap-self', userId: 'user-1' })).toEqual([
			ConnectAppsQueryKeyFactory.adminAppPasswords(),
			ConnectAppsQueryKeyFactory.appPasswords('user-1')
		]);
	});
});
