import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	discard: vi.fn(),
	operations: {
		data: { pages: [{ items: [] as Array<Record<string, unknown>> }] },
		isLoading: false,
		isError: false
	},
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
	},
	identityPreparations: {
		data: { pages: [{ items: [] as Array<Record<string, unknown>> }] },
		isLoading: false,
		isError: false
	},
	identityEstimate: {
		data: {
			album_count: 12,
			ready_album_count: 4,
			mapping_required_count: 6,
			exact_release_required_count: 2,
			selected_root_count: 0,
			queued_preparation_count: 0
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
		...h.operations
	}),
	getLibraryManagementRecoveryQuery: () => h.recovery
}));
vi.mock('$lib/queries/library-management/LibraryManagementMutations.svelte', () => ({
	controlLibraryManagementOperationMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	discardLibraryManagementPreviewMutation: () => ({ mutateAsync: h.discard, isPending: false }),
	createLibraryManagementPreviewMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createLibraryManagementBaselineRestorePreviewMutation: () => ({
		mutateAsync: vi.fn(),
		isPending: false
	})
}));
vi.mock('$lib/queries/library/LibraryIdentityPreparationQueries.svelte', () => ({
	getLibraryIdentityPreparationsQuery: () => h.identityPreparations,
	getLibraryIdentityPreparationEstimateQuery: () => h.identityEstimate,
	getLibraryIdentityPreparationFindingsQuery: () => ({
		data: { pages: [{ items: [] }] },
		isLoading: false,
		isError: false
	})
}));
vi.mock('$lib/queries/library/LibraryIdentityPreparationMutations.svelte', () => ({
	createLibraryIdentityPreparation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	applyLibraryIdentityPreparation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	discardLibraryIdentityPreparation: () => ({ mutateAsync: vi.fn(), isPending: false })
}));
vi.mock('$lib/queries/library/LibraryOperationMutations.svelte', () => ({
	controlLibraryOperation: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

import LibraryManagementControlRoom from './LibraryManagementControlRoom.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.operations = { data: { pages: [{ items: [] }] }, isLoading: false, isError: false };
	h.recovery.isError = false;
	h.identityPreparations = {
		data: { pages: [{ items: [] }] },
		isLoading: false,
		isError: false
	};
	h.discard.mockResolvedValue({});
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
		await expect.element(page.getByRole('heading', { name: 'Identity readiness' })).toBeVisible();
		await expect.element(page.getByText('Need exact track maps')).toBeVisible();
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

	it('confirms and discards a ready preview directly from its review card', async () => {
		h.operations = {
			data: {
				pages: [
					{
						items: [
							{
								operation: {
									id: 'preview-1',
									state: 'ready',
									row_revision: 7,
									updated_at: 1_800_000_000,
									failed_count: 0
								},
								profile_name: 'Picard-style Organizer',
								mode: 'preview',
								phase: 'ready'
							}
						]
					}
				]
			},
			isLoading: false,
			isError: false
		};
		render(LibraryManagementControlRoom);

		await page.getByRole('button', { name: 'Discard preview for Picard-style Organizer' }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Discard this preview?' }))
			.toHaveFocus();
		await expect.element(page.getByText(/No music file, tag, baseline/)).toBeVisible();
		await page.getByRole('button', { name: 'Discard preview', exact: true }).click();

		expect(h.discard).toHaveBeenCalledWith({
			jobId: 'preview-1',
			request: { expected_operation_row_revision: 7 }
		});
	});
});
