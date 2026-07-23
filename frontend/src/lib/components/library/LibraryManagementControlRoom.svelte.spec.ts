import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	recovery: {
		data: {
			recoverable_bundle_count: 0,
			nonterminal_journal_count: 0,
			needs_attention_count: 0,
			cleanup_pending_count: 0,
			oldest_updated_at: null,
			state_counts: {}
		},
		isLoading: false,
		isError: false
	}
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { isAdmin: true, user: { id: 'admin-1' } }
}));
vi.mock('$lib/queries/library/LibraryPolicyQueries.svelte', () => ({
	getTargetLibrarySettingsQuery: () => ({
		data: {
			policy_revision: 'policy-1',
			library_roots: [
				{ id: 'root-1', label: 'Archive', path: '/music', policy: 'automatic', rules: [] }
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
		data: { root_assignments: [], profiles: [], settings_revision: 'settings-1' },
		isLoading: false,
		isError: false
	}),
	getLibraryManagementOperationsQuery: () => ({
		data: { pages: [{ items: [] }] },
		isLoading: false,
		isError: false
	}),
	getLibraryManagementRecoveryQuery: () => h.recovery
}));
vi.mock('$lib/queries/library-management/LibraryManagementMutations.svelte', () => ({
	controlLibraryManagementOperationMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createLibraryManagementPreviewMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createLibraryManagementBaselineRestorePreviewMutation: () => ({
		mutateAsync: vi.fn(),
		isPending: false
	})
}));

import LibraryManagementControlRoom from './LibraryManagementControlRoom.svelte';

beforeEach(() => {
	h.recovery.isError = false;
});

describe('LibraryManagementControlRoom', () => {
	it('presents management as a separate opt-in write system', async () => {
		render(LibraryManagementControlRoom);
		await expect.element(page.getByRole('heading', { name: 'Library Management' })).toBeVisible();
		await expect.element(page.getByText('Separate write system')).toBeVisible();
		await expect
			.element(
				page.getByText(
					'Writes tags and organizes files. Scanning and identification above remain read-only.'
				)
			)
			.toBeVisible();
		await expect.element(page.getByText('Off everywhere')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Preview library management...' }))
			.toBeVisible();
	});

	it('fails closed visually when recovery diagnostics are unavailable', async () => {
		h.recovery.isError = true;
		render(LibraryManagementControlRoom);

		await expect.element(page.getByText('Status unavailable')).toBeVisible();
		await expect
			.element(page.getByRole('alert').getByText('Recovery status is unavailable'))
			.toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Preview library management...' }))
			.toBeDisabled();
	});
});
