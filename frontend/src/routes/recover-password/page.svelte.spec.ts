import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { AUTH_FREE_PATHS } from '$lib/constants';

const h = vi.hoisted(() => ({
	mutate: vi.fn().mockResolvedValue(undefined),
	reset: vi.fn()
}));

vi.mock('$lib/queries/auth/AuthMutations.svelte', () => ({
	createPasswordRecoveryResetMutation: () => ({
		mutateAsync: h.mutate,
		reset: h.reset,
		isPending: false
	})
}));

vi.mock('$lib/api/client', () => ({
	ApiError: class ApiError extends Error {}
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

	it('explains both administrator-issued and server-owner recovery', async () => {
		render(RecoveryPage);
		await expect
			.element(page.getByRole('heading', { name: 'Recover your local account' }))
			.toBeVisible();
		await page.getByText('Recover as server owner').click();
		await expect.element(page.getByText(/Only someone with host access/)).toBeVisible();
		await expect
			.element(page.getByText(/docker compose exec --user droppedneedle droppedneedle/))
			.toBeVisible();
		await expect.element(page.getByText(/Enter your username above/)).toBeVisible();
	});

	it('builds a safe host command from the entered username', async () => {
		render(RecoveryPage);
		await page.getByText('Recover as server owner').click();
		await expect.element(page.getByLabelText('Copy recovery command')).toBeDisabled();
		await page.getByLabelText('Username').fill('Alice.Admin');

		await expect.element(page.getByLabelText('Copy recovery command')).toBeEnabled();
		await expect.element(page.getByText(/recovery-code Alice\.Admin/)).toBeVisible();
	});

	it('submits the username, recovery code, and new password', async () => {
		render(RecoveryPage);
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

	it('rejects mismatched passwords before calling the server', async () => {
		render(RecoveryPage);
		await page.getByLabelText('Username').fill('Alice');
		await page.getByLabelText('Recovery code').fill('AAAA-BBBB-CCCC-DDDD-EEEE');
		await page.getByLabelText('New password').fill('a new secure password');
		await page.getByLabelText('Confirm password').fill('a different secure password');
		await page.getByRole('button', { name: 'Reset password' }).click();

		await expect.element(page.getByRole('alert')).toHaveTextContent('Passwords do not match');
		expect(h.mutate).not.toHaveBeenCalled();
	});

	it('rejects passwords over the bcrypt byte limit before calling the server', async () => {
		render(RecoveryPage);
		await page.getByLabelText('Username').fill('Alice');
		await page.getByLabelText('Recovery code').fill('AAAA-BBBB-CCCC-DDDD-EEEE');
		await page.getByLabelText('New password').fill('é'.repeat(37));
		await page.getByLabelText('Confirm password').fill('é'.repeat(37));
		await page.getByRole('button', { name: 'Reset password' }).click();

		await expect.element(page.getByRole('alert')).toHaveTextContent('72 UTF-8 bytes or fewer');
		expect(h.mutate).not.toHaveBeenCalled();
	});
});
