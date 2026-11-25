import { page } from 'vitest/browser';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import SettingsHealth from './SettingsHealth.svelte';

describe('the Health tab', () => {
	beforeEach(() =>
		authStore.setUser(
			toAuthUser({
				id: 'admin-1',
				display_name: 'Ada',
				role: 'admin',
				email: null,
				avatar_url: null,
				username: 'ada',
				username_display: 'Ada'
			})
		)
	);
	afterEach(() => authStore.clear());

	it('says nothing is wrong when the list is empty', async () => {
		fakeApi({ 'GET /api/v3/admin/problems': { checked_at: Date.now(), problems: [] } });
		await renderWithQuery(SettingsHealth, {});

		await expect.element(page.getByText('Nothing is wrong.')).toBeInTheDocument();
	});

	it('puts a broken row under Needs you with its fix', async () => {
		fakeApi({
			'GET /api/v3/admin/problems': {
				checked_at: Date.now(),
				problems: [
					{
						kind: 'publish_needs_attention',
						severity: 'broken',
						subject: null,
						count: 2,
						since_at: null,
						accept_folder: null,
						stale: false
					}
				]
			}
		});
		await renderWithQuery(SettingsHealth, {});

		await expect.element(page.getByRole('heading', { name: 'Needs you' })).toBeInTheDocument();
		await expect.element(page.getByText('2 file changes need you.')).toBeInTheDocument();
		await expect
			.element(page.getByRole('link', { name: 'Fix' }))
			.toHaveAttribute('href', expect.stringContaining('/library/management'));
	});
});
