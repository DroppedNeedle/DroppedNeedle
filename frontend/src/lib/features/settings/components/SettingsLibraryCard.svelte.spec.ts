import { page } from 'vitest/browser';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi, type FakeRoutes, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import SettingsLibraryCard from './SettingsLibraryCard.svelte';

const OVERLAP = 'Library folder Music overlaps the download folder /downloads.';
const MISSING = 'Library folder Archive does not exist.';

function routes(warnings: string[]): FakeRoutes {
	return {
		'GET /api/v3/settings/library': settings(warnings),
		'GET /api/v3/settings/library/policy-tree': { policy_revision: 'r1', roots: [], warnings: [] }
	};
}

function settings(warnings: string[]): RouteData<'GET /api/v3/settings/library'> {
	return {
		library_roots: [],
		staging_path: '',
		naming_script: '',
		acoustid_api_key: '',
		enabled: true,
		actions_applied: [],
		affected_scope_ids: [],
		policy_revision: 'r1',
		reconciliation_required: false,
		reconciliation_state: 'none',
		warnings
	};
}

describe('the library folders card', () => {
	beforeEach(() => {
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

	afterEach(() => authStore.clear());

	it('shows each standing warning on its own line', async () => {
		fakeApi(routes([OVERLAP, MISSING]));
		await renderWithQuery(SettingsLibraryCard, {});

		await expect.element(page.getByText(OVERLAP, { exact: true })).toBeVisible();
		await expect.element(page.getByText(MISSING, { exact: true })).toBeVisible();
	});

	it('shows no warning when there are none', async () => {
		fakeApi(routes([]));
		await renderWithQuery(SettingsLibraryCard, {});

		await expect.element(page.getByText('Local library', { exact: true })).toBeVisible();
		await expect.element(page.getByText(OVERLAP)).not.toBeInTheDocument();
		await expect.element(page.getByText(MISSING)).not.toBeInTheDocument();
	});
});
