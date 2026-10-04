import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
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
	],
	isPending: false,
	isError: false,
	error: null as Error | null,
	refetch: vi.fn(),
	revoke: vi.fn().mockResolvedValue(undefined),
	revokePending: false
}));

vi.mock('$lib/queries/sessions/SessionsQuery.svelte', () => ({
	getSessionsQuery: () => ({
		get data() {
			return h.isPending || h.isError ? undefined : { sessions: h.sessions };
		},
		get isPending() {
			return h.isPending;
		},
		get isError() {
			return h.isError;
		},
		get error() {
			return h.error;
		},
		refetch: h.refetch
	})
}));

vi.mock('$lib/queries/sessions/SessionsMutations.svelte', () => ({
	createRevokeSessionMutation: () => ({
		mutateAsync: h.revoke,
		get isPending() {
			return h.revokePending;
		}
	})
}));

import SessionsManager from './SessionsManager.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.isPending = false;
	h.isError = false;
	h.error = null;
	h.revokePending = false;
	h.sessions = [
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
	];
});

describe('SessionsManager', () => {
	it('lists sessions and marks the current one without a revoke control', async () => {
		await render(SessionsManager);

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
		await render(SessionsManager);

		await page.getByRole('button', { name: /revoke bedroom speaker/i }).click();
		await expect.element(page.getByRole('button', { name: 'Confirm revoke' })).toBeVisible();

		await page.getByRole('button', { name: 'Confirm revoke' }).click();

		expect(h.revoke).toHaveBeenCalledTimes(1);
		expect(h.revoke).toHaveBeenCalledWith({ id: 'sess-old', label: 'Bedroom speaker' });
	});

	it('lets the user back out of a revoke', async () => {
		await render(SessionsManager);

		await page.getByRole('button', { name: /revoke bedroom speaker/i }).click();
		await page.getByRole('button', { name: 'Keep session' }).click();

		expect(h.revoke).not.toHaveBeenCalled();
		await expect
			.element(page.getByRole('button', { name: /revoke bedroom speaker/i }))
			.toBeVisible();
	});

	it('shows a skeleton while loading', async () => {
		h.isPending = true;

		await render(SessionsManager);

		await expect.element(page.getByTestId('sessions-skeleton')).toBeInTheDocument();
		expect(page.getByText('Bedroom speaker').query()).toBeNull();
	});

	it('shows an honest error with a retry that refetches', async () => {
		h.isError = true;
		h.error = new Error('sessions unavailable');

		await render(SessionsManager);

		await expect.element(page.getByText(/could not load sessions/i)).toBeVisible();
		await page.getByRole('button', { name: /retry/i }).click();
		expect(h.refetch).toHaveBeenCalledTimes(1);
	});

	it('states plainly when no sessions exist', async () => {
		h.sessions = [];

		await render(SessionsManager);

		await expect.element(page.getByText(/no active sessions/i)).toBeVisible();
	});
});
