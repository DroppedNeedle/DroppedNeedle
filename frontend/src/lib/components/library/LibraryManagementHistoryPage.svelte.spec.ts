import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { isAdmin: true, user: { id: 'admin-1' } }
}));
vi.mock('$lib/queries/library/LibraryPolicyQueries.svelte', () => ({
	getTargetLibrarySettingsQuery: () => ({
		data: {
			library_roots: [
				{ id: 'root-1', label: 'Music', path: '/music', policy: 'automatic', rules: [] }
			]
		},
		isLoading: false,
		isError: false
	})
}));
vi.mock('$lib/queries/library-management/LibraryManagementEvents', () => ({
	createLibraryManagementEvents: () => ({ start: vi.fn(), stop: vi.fn() })
}));
vi.mock('$lib/queries/library-management/LibraryManagementQueries.svelte', () => ({
	getLibraryManagementSettingsQuery: () => ({
		data: { profiles: [] },
		isLoading: false,
		isError: false
	}),
	getLibraryManagementOperationsQuery: () => ({
		data: {
			pages: [
				{
					items: [
						{
							operation: {
								id: 'apply-1',
								state: 'succeeded',
								terminal_code: null,
								created_at: 1_785_265_474,
								updated_at: 1_785_318_922,
								succeeded_count: 1,
								failed_count: 0,
								skipped_count: 0
							},
							mode: 'apply',
							origin: 'manual',
							profile_name: 'Picard-style Organizer',
							selection: { kind: 'albums', ids: ['album-1'] },
							target_root_id: null
						},
						{
							operation: {
								id: 'preview-9',
								state: 'ready',
								terminal_code: null,
								created_at: 1_785_265_474,
								updated_at: 1_785_318_922,
								succeeded_count: 0,
								failed_count: 0,
								skipped_count: 0
							},
							mode: 'preview',
							origin: 'manual',
							profile_name: 'Ready Preview',
							selection: { kind: 'roots', ids: ['root-1'] },
							target_root_id: null,
							eligible_count: 4,
							warning_count: 2,
							blocked_count: 1,
							expires_at: 1_900_000_000
						}
					]
				}
			]
		},
		isLoading: false,
		isError: false,
		hasNextPage: false,
		isFetchingNextPage: false,
		fetchNextPage: vi.fn()
	})
}));

import LibraryManagementHistoryPage from './LibraryManagementHistoryPage.svelte';

describe('LibraryManagementHistoryPage', () => {
	it('labels both the start and finish time of completed work', async () => {
		render(LibraryManagementHistoryPage);

		const historyRow = page.getByRole('link', { name: /Picard-style Organizer/ });
		await expect.element(page.getByText(/Started .*Finished/)).toBeVisible();
		await expect.element(historyRow.getByText('Apply')).toBeVisible();
		await expect.element(historyRow.getByText('Succeeded', { exact: true })).toBeVisible();
	});
	it('shows outcome chips and an expiry badge on ready rows', async () => {
		render(LibraryManagementHistoryPage);

		const readyRow = page.getByRole('link', { name: /Ready Preview/ });
		await expect.element(readyRow.getByText('4 eligible')).toBeVisible();
		await expect.element(readyRow.getByText('2 warning')).toBeVisible();
		await expect.element(readyRow.getByText('1 blocked')).toBeVisible();
		await expect.element(readyRow.getByText(/Expires/)).toBeVisible();
		const doneRow = page.getByRole('link', { name: /Picard-style Organizer/ });
		await expect.element(doneRow.getByText('1 succeeded')).toBeVisible();
	});
});
