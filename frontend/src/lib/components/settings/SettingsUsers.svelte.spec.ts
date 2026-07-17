import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	get: vi.fn(),
	recoveryMutate: vi.fn(),
	recoveryReset: vi.fn(),
	toast: vi.fn()
}));

vi.mock('$lib/api/client', () => ({
	api: {
		get: (...args: unknown[]) => h.get(...args),
		post: vi.fn(),
		patch: vi.fn(),
		delete: vi.fn()
	}
}));

vi.mock('$lib/queries/auth/AuthMutations.svelte', () => ({
	createPasswordRecoveryCodeMutation: () => ({
		mutateAsync: h.recoveryMutate,
		reset: h.recoveryReset,
		isPending: false
	})
}));

vi.mock('$lib/queries/auth/ImportCandidatesQuery.svelte', () => ({
	getImportCandidatesQuery: () => ({ data: { users: [] }, isLoading: false, isError: false })
}));

vi.mock('$lib/queries/auth/UserImportMutations.svelte', () => ({
	createImportUsersMutation: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

vi.mock('$lib/queries/downloads/DownloadClientsQueries.svelte', () => ({
	getDownloadPolicyQuery: () => ({
		data: {
			default_request_quota_count: 0,
			default_request_quota_days: 7,
			default_storage_quota_gb: 0
		}
	}),
	saveDownloadPolicy: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'admin-1' } }
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: (...args: unknown[]) => h.toast(...args) }
}));

import SettingsUsers from './SettingsUsers.svelte';

beforeEach(() => {
	h.get.mockReset();
	h.get.mockResolvedValue({
		users: [
			{
				id: 'local-1',
				display_name: 'Local Listener',
				role: 'user',
				email: null,
				username: 'local',
				username_display: 'Local',
				avatar_url: null,
				providers: ['local']
			},
			{
				id: 'sso-1',
				display_name: 'SSO Listener',
				role: 'user',
				email: null,
				username: 'sso',
				username_display: 'SSO',
				avatar_url: null,
				providers: ['oidc']
			},
			{
				id: 'local-2',
				display_name: 'Second Listener',
				role: 'user',
				email: null,
				username: 'second',
				username_display: 'Second',
				avatar_url: null,
				providers: ['local']
			}
		],
		total: 3
	});
	h.recoveryMutate.mockReset();
	h.recoveryMutate.mockResolvedValue({
		recovery_code: 'AAAA-BBBB-CCCC-DDDD-EEEE',
		expires_at: '2026-07-17T17:00:00Z'
	});
	h.recoveryReset.mockReset();
	h.toast.mockClear();
});

describe('SettingsUsers password recovery', () => {
	it('creates and displays a one-time code for a local account', async () => {
		render(SettingsUsers);
		await expect.element(page.getByText('Local Listener')).toBeVisible();

		await page.getByLabelText('Create recovery code for Local Listener').click();
		await expect.element(page.getByRole('heading', { name: 'Create recovery code' })).toBeVisible();
		await page.getByRole('button', { name: 'Create code' }).click();

		expect(h.recoveryMutate).toHaveBeenCalledWith('local-1');
		await expect.element(page.getByText('AAAA-BBBB-CCCC-DDDD-EEEE')).toBeVisible();
		await expect.element(page.getByText(/will not be shown again/)).toBeVisible();
		await page.getByRole('button', { name: 'Done' }).click();
		expect(h.recoveryReset).toHaveBeenCalled();
	});

	it('disables DroppedNeedle recovery for an SSO-only account', async () => {
		render(SettingsUsers);
		await expect.element(page.getByText('SSO Listener')).toBeVisible();
		await expect
			.element(page.getByLabelText('Create recovery code for SSO Listener'))
			.toBeDisabled();
	});

	it('does not show a stale code after the dialog moves to another user', async () => {
		let resolveFirst: (value: { recovery_code: string; expires_at: string }) => void;
		h.recoveryMutate
			.mockImplementationOnce(
				() =>
					new Promise((resolve) => {
						resolveFirst = resolve;
					})
			)
			.mockResolvedValueOnce({
				recovery_code: '2222-2222-2222-2222-2222',
				expires_at: '2026-07-17T17:00:00Z'
			});

		render(SettingsUsers);
		await expect.element(page.getByText('Local Listener')).toBeVisible();
		await page.getByLabelText('Create recovery code for Local Listener').click();
		await page.getByRole('button', { name: 'Create code' }).click();
		(page.getByRole('dialog').element() as HTMLDialogElement).close();

		await page.getByLabelText('Create recovery code for Second Listener').click();
		await page.getByRole('button', { name: 'Create code' }).click();
		await expect.element(page.getByText('2222-2222-2222-2222-2222')).toBeVisible();

		resolveFirst!({
			recovery_code: '1111-1111-1111-1111-1111',
			expires_at: '2026-07-17T17:00:00Z'
		});
		await new Promise((resolve) => setTimeout(resolve, 0));
		await expect.element(page.getByText('1111-1111-1111-1111-1111')).not.toBeInTheDocument();
		await expect.element(page.getByText('2222-2222-2222-2222-2222')).toBeVisible();
	});
});
