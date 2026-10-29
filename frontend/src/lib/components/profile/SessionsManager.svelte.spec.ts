import { page } from 'vitest/browser';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { apiError, fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

import { goto } from '$app/navigation';
import { authStore, LAST_USER_ID_KEY } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/queries/auth/types';
import SessionsManager from './SessionsManager.svelte';

const sessions: RouteData<'GET /api/v3/auth/sessions'> = {
	sessions: [
		{
			id: 'sess-current',
			kind: 'standard',
			label: 'Firefox on Linux',
			created_at: 1759400000,
			last_seen_at: 1759403600,
			expires_at: 1762000000,
			current: true
		},
		{
			id: 'sess-old',
			kind: 'companion',
			label: 'Bedroom speaker',
			created_at: 1759300000,
			last_seen_at: 1759303600,
			expires_at: 1762000000,
			current: false
		}
	]
};

beforeEach(() => {
	authStore.setUser(
		toAuthUser({
			id: 'user-1',
			display_name: 'Someone',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'someone',
			username_display: 'Someone'
		})
	);
	localStorage.setItem(LAST_USER_ID_KEY, 'user-1');
});

afterEach(() => {
	authStore.clear();
	localStorage.clear();
	vi.mocked(goto).mockClear();
});

describe('SessionsManager', () => {
	it('lists sessions and marks the current one without a revoke control', async () => {
		fakeApi({ 'GET /api/v3/auth/sessions': sessions });
		await renderWithQuery(SessionsManager, {});

		await expect.element(page.getByText('Firefox on Linux')).toBeVisible();
		await expect.element(page.getByText('Bedroom speaker')).toBeVisible();
		await expect.element(page.getByText('This session')).toBeVisible();
		expect(page.getByRole('button', { name: /revoke firefox on linux/i }).elements()).toHaveLength(
			0
		);
		await expect
			.element(page.getByRole('button', { name: /revoke bedroom speaker/i }))
			.toBeVisible();
	});

	it('revokes another session after an inline confirm', async () => {
		const api = fakeApi({
			'GET /api/v3/auth/sessions': sessions,
			'DELETE /api/v3/auth/sessions/{id}': undefined
		});
		await renderWithQuery(SessionsManager, {});

		await page.getByRole('button', { name: /revoke bedroom speaker/i }).click();
		await page.getByRole('button', { name: 'Confirm revoke' }).click();

		await expect.poll(() => api.calls('DELETE /api/v3/auth/sessions/{id}').length).toBe(1);
		expect(api.calls('DELETE /api/v3/auth/sessions/{id}')[0]?.params).toEqual({ id: 'sess-old' });
	});

	it('lets the user back out of a revoke', async () => {
		const api = fakeApi({ 'GET /api/v3/auth/sessions': sessions });
		await renderWithQuery(SessionsManager, {});

		await page.getByRole('button', { name: /revoke bedroom speaker/i }).click();
		await page.getByRole('button', { name: 'Keep session' }).click();

		await expect
			.element(page.getByRole('button', { name: /revoke bedroom speaker/i }))
			.toBeVisible();
		expect(api.calls('DELETE /api/v3/auth/sessions/{id}')).toHaveLength(0);
	});

	it('shows an honest error with a retry that reads again', async () => {
		let reads = 0;
		fakeApi({
			'GET /api/v3/auth/sessions': () => {
				reads += 1;
				return reads === 1 ? apiError(503, 'SERVER_BUSY') : sessions;
			}
		});
		await renderWithQuery(SessionsManager, {});

		await expect.element(page.getByText(/could not load sessions/i)).toBeVisible();
		await page.getByRole('button', { name: /retry/i }).click();
		await expect.element(page.getByText('Bedroom speaker')).toBeVisible();
	});

	it('signs out everywhere, clears this browser and opens the sign-in page', async () => {
		const api = fakeApi({
			'GET /api/v3/auth/sessions': sessions,
			'POST /api/v3/auth/logout-all': undefined
		});
		await renderWithQuery(SessionsManager, {});

		await page.getByRole('button', { name: 'Sign out everywhere' }).click();
		await expect.element(page.getByText(/app passwords keep working/i)).toBeVisible();
		await page.getByRole('dialog').getByRole('button', { name: 'Sign out everywhere' }).click();

		await expect.poll(() => vi.mocked(goto).mock.calls.length).toBe(1);
		expect(vi.mocked(goto).mock.calls[0]?.[0]).toMatch(/\/login$/);
		expect(api.calls('POST /api/v3/auth/logout-all')).toHaveLength(1);
		expect(authStore.user).toBeNull();
		expect(localStorage.getItem(LAST_USER_ID_KEY)).toBeNull();
	});
});
