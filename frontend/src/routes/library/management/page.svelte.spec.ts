import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

vi.mock('$app/state', () => ({
	page: { url: new URL('http://localhost/library/management?tab=organize') }
}));
vi.mock('$app/navigation', () => ({
	goto: vi.fn(),
	replaceState: vi.fn(),
	pushState: vi.fn(),
	beforeNavigate: vi.fn(),
	afterNavigate: vi.fn(),
	onNavigate: vi.fn(),
	invalidate: vi.fn(),
	invalidateAll: vi.fn()
}));

import ManagementPage from './+page.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/queries/auth/types';

const librarySwitchedOff: RouteData<'GET /api/v3/settings/library'> = {
	acoustid_api_key: '',
	enabled: false,
	library_roots: [{ id: 'music', label: 'Music', path: '/music', policy: 'automatic', rules: [] }],
	naming_script: '',
	staging_path: '',
	actions_applied: [],
	affected_scope_ids: [],
	policy_revision: 'policy-1',
	reconciliation_required: false,
	reconciliation_state: 'applied',
	warnings: []
};

const noActivity: RouteData<'GET /api/v3/library/activity'> = {
	items: [],
	revisions: {},
	work_items: []
};

beforeEach(() => {
	localStorage.clear();
	authStore.setUser(
		toAuthUser({
			id: 'admin-1',
			display_name: 'Admin',
			role: 'admin',
			email: null,
			avatar_url: null,
			username: 'admin',
			username_display: 'Admin'
		})
	);
});

describe('library management page', () => {
	it('says why nothing can be organized while the library is off', async () => {
		fakeApi({
			'GET /api/v3/settings/library': librarySwitchedOff,
			'GET /api/v3/library/activity': noActivity
		});
		await renderWithQuery(ManagementPage, {});

		await expect.element(page.getByText('The local library is disabled')).toBeVisible();
		await expect
			.element(page.getByText('No files are changed while it is off.', { exact: false }))
			.toBeVisible();
	});
});
