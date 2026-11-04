import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { AUTH_FREE_PATHS } from '$lib/constants';

const h = vi.hoisted(() => ({
	mutate: vi.fn().mockResolvedValue(undefined),
	reset: vi.fn()
}));

vi.mock('$lib/features/auth/queries/AuthMutations.svelte', () => ({
	createPasswordRecoveryResetMutation: () => ({
		mutateAsync: h.mutate,
		reset: h.reset,
		isPending: false
	})
}));

import RecoveryPage from './+page.svelte';

beforeEach(() => {
	h.mutate.mockReset();
	h.mutate.mockResolvedValue(undefined);
	h.reset.mockReset();
});

describe('password recovery page', () => {
	it('is available without an authenticated session', () => {
		expect(AUTH_FREE_PATHS).toContain('/recover-password');
	});

	it('explains both kinds of recovery and builds a safe host command from the username', async () => {
		await render(RecoveryPage);
		await expect
			.element(page.getByRole('heading', { name: 'Recover your local account' }))
			.toBeVisible();
		await page.getByText('Recover as server owner').click();
		await expect.element(page.getByText(/Only someone with host access/)).toBeVisible();
		await expect.element(page.getByText(/Stop the server first/)).toBeVisible();
		await expect
			.element(page.getByText(/droppedneedle-tool recovery-code YOUR_USERNAME/))
			.toBeVisible();
		await expect.element(page.getByText(/recovery-code --list-admins/)).toBeVisible();
		await expect.element(page.getByText(/Enter your username above/)).toBeVisible();
		await expect.element(page.getByLabelText('Copy recovery command')).toBeDisabled();

		await page.getByLabelText('Username').fill('Alice.Admin');

		await expect.element(page.getByLabelText('Copy recovery command')).toBeEnabled();
		await expect.element(page.getByText(/recovery-code Alice\.Admin/)).toBeVisible();
	});

	it('submits the username, recovery code, and new password', async () => {
		await render(RecoveryPage);
		await page.getByLabelText('Username').fill('Alice');
		await page.getByLabelText('Recovery code').fill('AAAA-BBBB-CCCC-DDDD-EEEE');
		await page.getByLabelText('New password').fill('a new secure password');
		await page.getByLabelText('Confirm password').fill('a new secure password');
		await page.getByRole('button', { name: 'Reset password' }).click();

		expect(h.mutate).toHaveBeenCalledWith({
			username: 'Alice',
			recovery_code: 'AAAA-BBBB-CCCC-DDDD-EEEE',
			new_password: 'a new secure password'
		});
		expect(h.reset).toHaveBeenCalledOnce();
		await expect.element(page.getByRole('heading', { name: 'Password changed' })).toBeVisible();
	});

	it('refuses mismatched or over-long passwords before calling the server', async () => {
		await render(RecoveryPage);
		await page.getByLabelText('Username').fill('Alice');
		await page.getByLabelText('Recovery code').fill('AAAA-BBBB-CCCC-DDDD-EEEE');
		for (const [password, confirm, alert] of [
			['a new secure password', 'a different secure password', 'Passwords do not match'],
			['é'.repeat(257), 'é'.repeat(257), '512 bytes or fewer']
		]) {
			await page.getByLabelText('New password').fill(password);
			await page.getByLabelText('Confirm password').fill(confirm);
			await page.getByRole('button', { name: 'Reset password' }).click();

			await expect.element(page.getByRole('alert')).toHaveTextContent(alert);
		}
		expect(h.mutate).not.toHaveBeenCalled();
	});
});
