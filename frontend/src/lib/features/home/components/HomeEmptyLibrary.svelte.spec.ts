import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it } from 'vitest';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import HomeEmptyLibrary from './HomeEmptyLibrary.svelte';

const library: RouteData<'GET /api/v3/settings/library'> = {
	library_roots: [{ id: 'root-1', label: 'Music', path: '/music', policy: 'automatic', rules: [] }],
	staging_path: '',
	naming_script: '',
	acoustid_api_key: '',
	enabled: true,
	actions_applied: [],
	affected_scope_ids: [],
	policy_revision: 'r1',
	reconciliation_required: false,
	reconciliation_state: 'applied',
	warnings: []
};

const queued: RouteData<'POST /api/v3/library/scan/runs'> = {
	conflicting_kind: null,
	disposition: 'started',
	queued_reason: null,
	row_revision: 1,
	run_id: 'run-1',
	state: 'queued'
};

beforeEach(() => {
	localStorage.clear();
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
	);
});

describe('Home with an empty library', () => {
	it('starts a scan of every library folder when an admin presses Start scan', async () => {
		const api = fakeApi({
			'GET /api/v3/settings/library': library,
			'POST /api/v3/library/scan/runs': queued
		});
		await renderWithQuery(HomeEmptyLibrary, {});

		await page.getByRole('button', { name: 'Start scan' }).click();

		await expect.poll(() => api.calls('POST /api/v3/library/scan/runs').length).toBe(1);
		expect(api.calls('POST /api/v3/library/scan/runs')[0]?.body).toEqual({
			kind: 'incremental',
			scope_ids: [],
			expected_policy_revision: 'r1'
		});
	});

	it('leads to Settings > Library while there is no library folder', async () => {
		const api = fakeApi({
			'GET /api/v3/settings/library': { ...library, library_roots: [] },
			'POST /api/v3/library/scan/runs': queued
		});
		await renderWithQuery(HomeEmptyLibrary, {});

		const link = page.getByRole('link', { name: 'Add a library folder' });
		await expect.element(link).toHaveAttribute('href', expect.stringContaining('tab=library'));
		expect(api.calls('POST /api/v3/library/scan/runs')).toHaveLength(0);
	});
});
