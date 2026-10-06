import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ConnectAppsQueryKeyFactory } from './ConnectAppsQueryKeyFactory';

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: vi.fn(), POST: vi.fn(), PUT: vi.fn(), DELETE: vi.fn() } } }
}));

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn()
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-1' } }
}));

import { api } from '$lib/api/client';
import { createMutation } from '@tanstack/svelte-query';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';

const mockPut = vi.mocked(api.global.v3.PUT) as unknown as ReturnType<typeof vi.fn>;
const mockPost = vi.mocked(api.global.v3.POST) as unknown as ReturnType<typeof vi.fn>;
const mockDelete = vi.mocked(api.global.v3.DELETE) as unknown as ReturnType<typeof vi.fn>;
const mockCreateMutation = vi.mocked(createMutation);
const mockInvalidate = vi.mocked(invalidateQueriesWithPersister);

beforeEach(() => vi.clearAllMocks());

function lastMutationOpts(): Record<string, unknown> {
	const calls = mockCreateMutation.mock.calls;
	return (calls[calls.length - 1][0] as unknown as () => Record<string, unknown>)();
}

describe('ConnectAppsQueryKeyFactory', () => {
	it('scopes the app-passwords key by userId and namespaces the admin roster', () => {
		expect(ConnectAppsQueryKeyFactory.settings()).toEqual(['connect-apps', 'settings']);
		expect(ConnectAppsQueryKeyFactory.appPasswords('user-1')).toEqual([
			'connect-apps',
			'user-1',
			'app-passwords'
		]);
		expect(ConnectAppsQueryKeyFactory.adminAppPasswords()).toEqual([
			'connect-apps',
			'admin',
			'app-passwords'
		]);
	});
});

describe('saveConnectAppsSettings', () => {
	it('PUTs the settings and invalidates the settings key', async () => {
		mockPut.mockResolvedValue({});
		const { saveConnectAppsSettings } = await import('./ConnectAppsMutations.svelte');
		saveConnectAppsSettings();
		const opts = lastMutationOpts();
		await (opts.mutationFn as (v: unknown) => Promise<unknown>)({ subsonic_enabled: true });
		expect(mockPut).toHaveBeenCalledWith('/api/v3/settings/connect-apps', {
			subsonic_enabled: true
		});
		await (opts.onSuccess as () => Promise<unknown>)();
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: ConnectAppsQueryKeyFactory.settings()
		});
	});
});

describe('createAppPassword', () => {
	it("POSTs {name} and invalidates the caller's app-passwords list", async () => {
		mockPost.mockResolvedValue({ secret: 's', app_password: {} });
		const { createAppPassword } = await import('./ConnectAppsMutations.svelte');
		createAppPassword();
		const opts = lastMutationOpts();
		await (opts.mutationFn as (v: string) => Promise<unknown>)('Symfonium (phone)');
		expect(mockPost).toHaveBeenCalledWith('/api/v3/me/app-passwords', {
			name: 'Symfonium (phone)'
		});
		await (opts.onSuccess as () => Promise<unknown>)();
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: ConnectAppsQueryKeyFactory.appPasswords('user-1')
		});
	});
});

describe('revokeAppPassword', () => {
	it("DELETEs by id and invalidates the caller's app-passwords list", async () => {
		mockDelete.mockResolvedValue(undefined);
		const { revokeAppPassword } = await import('./ConnectAppsMutations.svelte');
		revokeAppPassword();
		const opts = lastMutationOpts();
		await (opts.mutationFn as (v: string) => Promise<unknown>)('ap-1');
		expect(mockDelete).toHaveBeenCalledWith('/api/v3/me/app-passwords/ap-1');
		await (opts.onSuccess as () => Promise<unknown>)();
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: ConnectAppsQueryKeyFactory.appPasswords('user-1')
		});
	});
});

describe('adminRevokeAppPassword', () => {
	it('DELETEs the admin endpoint and invalidates the roster only (other user)', async () => {
		mockDelete.mockResolvedValue(undefined);
		const { adminRevokeAppPassword } = await import('./ConnectAppsMutations.svelte');
		adminRevokeAppPassword();
		const opts = lastMutationOpts();
		await (opts.mutationFn as (v: { id: string; userId: string }) => Promise<unknown>)({
			id: 'ap-9',
			userId: 'user-bob'
		});
		expect(mockDelete).toHaveBeenCalledWith('/api/v3/admin/app-passwords/ap-9');
		(opts.onSuccess as (d: unknown, v: { id: string; userId: string }) => void)(undefined, {
			id: 'ap-9',
			userId: 'user-bob'
		});
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: ConnectAppsQueryKeyFactory.adminAppPasswords()
		});
		// another user's revoke must NOT touch the admin's own per-user key
		expect(mockInvalidate).not.toHaveBeenCalledWith({
			queryKey: ConnectAppsQueryKeyFactory.appPasswords('user-1')
		});
	});

	it("also refreshes the admin's own list when they revoke their own password", async () => {
		mockDelete.mockResolvedValue(undefined);
		const { adminRevokeAppPassword } = await import('./ConnectAppsMutations.svelte');
		adminRevokeAppPassword();
		const opts = lastMutationOpts();
		(opts.onSuccess as (d: unknown, v: { id: string; userId: string }) => void)(undefined, {
			id: 'ap-self',
			userId: 'user-1'
		});
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: ConnectAppsQueryKeyFactory.adminAppPasswords()
		});
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: ConnectAppsQueryKeyFactory.appPasswords('user-1')
		});
	});
});
