import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	discard: vi.fn(),
	reissue: vi.fn(),
	resolveImportBundle: vi.fn(),
	goto: vi.fn(),
	replaceState: vi.fn(),
	apiGet: vi.fn(),
	apiPost: vi.fn(),
	toast: vi.fn(),
	invalidate: vi.fn(),
	appPage: {
		url: new URL('https://music.example.test/library/management#management-controls'),
		state: {}
	},
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
			state_counts: {},
			needs_attention_bundles: [] as Array<{ bundle_id: string }>
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
	},
	admin: true
}));

vi.mock('$app/navigation', () => ({ goto: h.goto, replaceState: h.replaceState }));
vi.mock('$app/state', () => ({ page: h.appPage }));
vi.mock('$lib/api/client', () => ({
	api: { global: { get: h.apiGet, post: h.apiPost } },
	ApiError: class ApiError extends Error {}
}));
vi.mock('$lib/stores/toast', () => ({ toastStore: { show: h.toast } }));
vi.mock('$lib/queries/library-management/LibraryManagementInvalidation', () => ({
	invalidateLibraryManagementSurfaces: h.invalidate
}));
vi.mock('$lib/stores/authStore.svelte', () => ({
	LAST_USER_ID_KEY: 'test:last-user',
	authStore: {
		get isAdmin() {
			return h.admin;
		},
		user: { id: 'admin-1' }
	}
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
	}),
	getLibraryPolicyTreeQuery: () => ({
		data: { policy_revision: 'policy-1', roots: [] },
		isSuccess: true,
		isLoading: false,
		isError: false
	})
}));
vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibrarySearchQuery: () => ({ data: { artists: [], albums: [], tracks: [] } }),
	getLibraryAlbumDetailQuery: () => ({ data: undefined, isLoading: false, isError: false })
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
	reissueLibraryManagementPreviewMutation: () => ({ mutateAsync: h.reissue, isPending: false }),
	resolveLibraryManagementImportBundleMutation: () => ({
		mutateAsync: h.resolveImportBundle,
		isPending: false
	}),
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
	discardLibraryIdentityPreparation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	undoLibraryAutomaticEdition: () => ({ mutateAsync: vi.fn(), isPending: false })
}));
vi.mock('$lib/queries/library/LibraryOperationMutations.svelte', () => ({
	controlLibraryOperation: () => ({ mutateAsync: vi.fn(), isPending: false })
}));
vi.mock('$lib/queries/library/LibraryRepairQueries.svelte', () => ({
	getLibraryRepairsQuery: () => ({ data: { pages: [{ items: [] }] }, isLoading: false }),
	getLibraryRepairEstimateQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryRepairFindingsQuery: () => ({
		data: { pages: [{ items: [] }] },
		isLoading: false,
		isError: false,
		hasNextPage: false
	})
}));
vi.mock('$lib/queries/library/LibraryRepairMutations.svelte', () => ({
	createLibraryRepair: () => ({ mutateAsync: vi.fn(), isPending: false }),
	applyLibraryRepair: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

import LibraryManagementControlRoom from './LibraryManagementControlRoom.svelte';

function failedOperation(
	id: string,
	overrides: Record<string, unknown> = {},
	operationOverrides: Record<string, unknown> = {}
): Record<string, unknown> {
	return {
		operation: {
			id,
			state: 'failed',
			terminal_code: 'STALE_INPUT',
			row_revision: 3,
			updated_at: 1_800_000_001,
			succeeded_count: 0,
			failed_count: 0,
			skipped_count: 0,
			...operationOverrides
		},
		profile_name: 'Picard-style Organizer',
		mode: 'preview',
		phase: 'planning',
		selection: { kind: 'albums', ids: ['album-1'] },
		...overrides
	};
}

function historyWith(items: Array<Record<string, unknown>>): {
	data: { pages: Array<{ items: Array<Record<string, unknown>> }> };
	isLoading: boolean;
	isError: boolean;
} {
	return { data: { pages: [{ items }] }, isLoading: false, isError: false };
}

function recoveryDataWith(overrides: Partial<typeof h.recovery.data> = {}): typeof h.recovery.data {
	return {
		recoverable_bundle_count: 0,
		nonterminal_journal_count: 0,
		needs_attention_count: 0,
		cleanup_pending_count: 0,
		oldest_updated_at: null,
		state_counts: {},
		needs_attention_bundles: [],
		...overrides
	};
}

beforeEach(() => {
	vi.clearAllMocks();
	h.appPage.url = new URL('https://music.example.test/library/management#management-controls');
	h.operations = { data: { pages: [{ items: [] }] }, isLoading: false, isError: false };
	h.recovery.data = recoveryDataWith();
	h.recovery.isLoading = false;
	h.recovery.isError = false;
	h.admin = true;
	h.resolveImportBundle.mockResolvedValue({
		bundle_id: 'bundle-1',
		state: 'resolved',
		verified_files: 4,
		total_files: 4
	});
	h.apiPost.mockResolvedValue({});
	h.reissue.mockResolvedValue({ job_id: 'reissued-1', preview_token: 'token-1' });
	h.invalidate.mockResolvedValue(undefined);
	h.identityPreparations = {
		data: { pages: [{ items: [] }] },
		isLoading: false,
		isError: false
	};
	h.discard.mockResolvedValue({});
});

describe('LibraryManagementControlRoom', () => {
	it('presents organization as a separate opt-in write system', async () => {
		await render(LibraryManagementControlRoom);
		await expect.element(page.getByRole('heading', { name: 'Organize files' })).toBeVisible();
		await expect
			.element(
				page.getByText(
					'Writes tags and organizes files on disk - nothing changes until you review and apply a preview.'
				)
			)
			.toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Automation' }))
			.toHaveAttribute('href', '/library/management?tab=automation');
		await expect.element(page.getByText('Off everywhere')).toBeVisible();
		await expect.element(page.getByRole('heading', { name: 'Identity readiness' })).toBeVisible();
		await expect.element(page.getByRole('heading', { name: 'Repair' })).toBeVisible();
		await expect.element(page.getByText('Need exact track maps')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Preview organization...' }))
			.toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Restore original state...' }))
			.toBeVisible();
	});

	it('fails closed visually when recovery diagnostics are unavailable', async () => {
		h.recovery.isError = true;
		await render(LibraryManagementControlRoom);

		await expect.element(page.getByText('Status unavailable')).toBeVisible();
		await expect
			.element(page.getByRole('alert').getByText('Recovery status is unavailable'))
			.toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Preview organization...' }))
			.toBeDisabled();
	});

	it('opens a deep-linked baseline restore and cleans the URL when closed', async () => {
		h.appPage.url = new URL(
			'https://music.example.test/library/management?runner=baseline_restore#management-controls'
		);
		await render(LibraryManagementControlRoom);

		await expect
			.element(page.getByRole('heading', { name: 'Restore original state' }))
			.toHaveFocus();
		await page.getByRole('button', { name: 'Close manual management runner' }).click();

		expect(h.replaceState).toHaveBeenCalledOnce();
		const [url, state] = h.replaceState.mock.calls[0] as [URL, Record<string, unknown>];
		expect(url.pathname + url.search + url.hash).toBe('/library/management#management-controls');
		expect(state).toBe(h.appPage.state);
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
		await render(LibraryManagementControlRoom);

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

	it('does not present an activation dry run as an awaiting manual preview', async () => {
		h.operations = {
			data: {
				pages: [
					{
						items: [
							{
								operation: {
									id: 'activation-preview-1',
									state: 'ready',
									row_revision: 7,
									updated_at: 1_800_000_000,
									failed_count: 0
								},
								profile_name: 'Picard-style Organizer',
								mode: 'preview',
								phase: 'ready',
								activation_preview: true
							}
						]
					}
				]
			},
			isLoading: false,
			isError: false
		};
		await render(LibraryManagementControlRoom);

		await expect.element(page.getByText('Ready previews')).toBeVisible();
		await expect
			.element(page.getByRole('heading', { name: 'Ready previews' }))
			.not.toBeInTheDocument();
		await expect
			.element(page.getByRole('button', { name: /Discard preview/ }))
			.not.toBeInTheDocument();
	});

	it('describes unknown planning scope without presenting false zero progress', async () => {
		h.operations = {
			data: {
				pages: [
					{
						items: [
							{
								operation: {
									id: 'activation-preview-1',
									state: 'running',
									row_revision: 2,
									completed_count: 0,
									expected_work_count: 0,
									failed_count: 0
								},
								profile_name: 'Picard-style Organizer + Lyrics',
								mode: 'preview',
								phase: 'planning',
								activation_preview: true
							}
						]
					}
				]
			},
			isLoading: false,
			isError: false
		};

		await render(LibraryManagementControlRoom);

		await expect.element(page.getByText('Write-access dry run')).toBeVisible();
		await expect.element(page.getByText(/Discovering files and release bundles/)).toBeVisible();
		await expect.element(page.getByText('0 / 0')).not.toBeInTheDocument();
		await expect
			.element(page.getByRole('link', { name: 'Open details' }))
			.toHaveAttribute('href', '/library/management/previews/activation-preview-1');
	});
	it('shows outcome chips on ready previews and recent work', async () => {
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
									succeeded_count: 0,
									failed_count: 0,
									skipped_count: 0
								},
								profile_name: 'Picard-style Organizer',
								mode: 'preview',
								phase: 'ready',
								eligible_count: 4,
								warning_count: 2,
								blocked_count: 1
							},
							{
								operation: {
									id: 'apply-1',
									state: 'succeeded',
									row_revision: 3,
									updated_at: 1_800_000_000,
									succeeded_count: 5,
									failed_count: 1,
									skipped_count: 2
								},
								profile_name: 'Picard-style Organizer',
								mode: 'apply',
								phase: 'applying'
							}
						]
					}
				]
			},
			isLoading: false,
			isError: false
		};
		await render(LibraryManagementControlRoom);

		await expect.element(page.getByText('4 eligible')).toBeVisible();
		await expect.element(page.getByText('2 warning')).toBeVisible();
		await expect.element(page.getByText('1 blocked')).toBeVisible();
		await expect.element(page.getByText('5 succeeded')).toBeVisible();
		await expect.element(page.getByText('1 failed')).toBeVisible();
		await expect.element(page.getByText('2 skipped')).toBeVisible();
	});

	it('renders a STALE_INPUT terminal as superseded, not as needs-attention', async () => {
		h.operations = historyWith([failedOperation('stale-1', {}, { failed_count: 1 })]);
		await render(LibraryManagementControlRoom);

		await expect.element(page.getByText('Superseded · inputs moved')).toBeVisible();
		await expect.element(page.getByText(/Inputs moved since planning/)).toBeVisible();
		await expect.element(page.getByText('1 failed')).toHaveClass(/badge-ghost/);
		await expect.element(page.getByText('1 failed')).not.toHaveClass(/badge-error/);
		await expect.element(page.getByText('Failed', { exact: true })).not.toBeInTheDocument();
		expect(page.getByText('0', { exact: true }).elements()).toHaveLength(2);
	});

	it('collapses duplicate failed cards for the same album into one group with a count', async () => {
		h.operations = historyWith([failedOperation('stale-1'), failedOperation('stale-2')]);
		await render(LibraryManagementControlRoom);

		await expect.element(page.getByText('2 failed attempts · same album')).toBeVisible();
		await expect.element(page.getByText('Superseded', { exact: true })).toBeVisible();
		expect(page.getByText('Picard-style Organizer').elements()).toHaveLength(1);
		expect(page.getByRole('link', { name: 'Open details' }).elements()).toHaveLength(2);
	});

	it('bulk-retries only the stale members with one toast summary', async () => {
		h.operations = historyWith([failedOperation('stale-1'), failedOperation('stale-2')]);
		await render(LibraryManagementControlRoom);

		await page.getByRole('button', { name: 'Retry 2 stale' }).click();

		await expect.element(page.getByText('Retried 2 of 2 stale previews.')).toBeVisible();
		expect(h.reissue).toHaveBeenCalledTimes(2);
		expect(h.reissue).toHaveBeenNthCalledWith(1, { jobId: 'stale-1', silent: true });
		expect(h.reissue).toHaveBeenNthCalledWith(2, { jobId: 'stale-2', silent: true });
		expect(h.toast).toHaveBeenCalledTimes(1);
		expect(h.toast).toHaveBeenCalledWith({
			message: 'Retried 2 of 2 stale previews.',
			type: 'success'
		});
	});

	it('tolerates per-item bulk errors with a count in the summary', async () => {
		h.operations = historyWith([failedOperation('stale-1'), failedOperation('stale-2')]);
		h.reissue.mockRejectedValueOnce(new Error('gone'));
		await render(LibraryManagementControlRoom);

		await page.getByRole('button', { name: 'Retry 2 stale' }).click();

		await expect.element(page.getByText('Retried 1 of 2 stale previews (1 failed).')).toBeVisible();
		expect(h.toast).toHaveBeenCalledTimes(1);
		expect(h.toast).toHaveBeenCalledWith({
			message: 'Retried 1 of 2 stale previews (1 failed).',
			type: 'error'
		});
	});

	it('never bulk-retries real failures mixed in with stale attempts', async () => {
		h.operations = historyWith([
			failedOperation('stale-1'),
			failedOperation('real-1', {}, { terminal_code: 'PLANNING_FAILED', failed_count: 1 })
		]);
		await render(LibraryManagementControlRoom);

		// The sentence spans a source line break Svelte does not join for text
		// matching, so assert both halves instead of one spanning expression.
		await expect.element(page.getByText(/attempts only found moved/)).toBeVisible();
		await expect.element(page.getByText(/inputs and can be retried/)).toBeVisible();
		await expect.element(page.getByText('PLANNING FAILED')).toBeVisible();
		await expect.element(page.getByText('1', { exact: true })).toBeVisible();
		await page.getByRole('button', { name: 'Retry 1 stale' }).click();

		await expect.element(page.getByText('Retried 1 of 1 stale preview.')).toBeVisible();
		expect(h.reissue).toHaveBeenCalledTimes(1);
		expect(h.reissue).toHaveBeenCalledWith({ jobId: 'stale-1', silent: true });
		expect(h.toast).toHaveBeenCalledTimes(1);
	});

	it('bulk-dismisses stale previews with their expected revisions', async () => {
		h.operations = historyWith([failedOperation('stale-1'), failedOperation('stale-2')]);
		await render(LibraryManagementControlRoom);

		await page.getByRole('button', { name: 'Dismiss 2 stale' }).click();

		await expect.element(page.getByText('Dismissed 2 of 2 stale previews.')).toBeVisible();
		expect(h.discard).toHaveBeenCalledTimes(2);
		expect(h.discard).toHaveBeenCalledWith({
			jobId: 'stale-1',
			request: { expected_operation_row_revision: 3 },
			silent: true
		});
		expect(h.discard).toHaveBeenCalledWith({
			jobId: 'stale-2',
			request: { expected_operation_row_revision: 3 },
			silent: true
		});
		expect(h.toast).toHaveBeenCalledTimes(1);
	});

	it('hides bulk stale actions from non-admins', async () => {
		h.admin = false;
		h.operations = historyWith([failedOperation('stale-1'), failedOperation('stale-2')]);
		await render(LibraryManagementControlRoom);

		await expect.element(page.getByText('2 failed attempts · same album')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: /Retry .* stale/ }))
			.not.toBeInTheDocument();
		await expect
			.element(page.getByRole('button', { name: /Dismiss .* stale/ }))
			.not.toBeInTheDocument();
	});

	it('marks a stuck import bundle as handled and refreshes recovery state', async () => {
		h.recovery.data = recoveryDataWith({
			needs_attention_count: 1,
			needs_attention_bundles: [{ bundle_id: 'bundle-1' }]
		});
		await render(LibraryManagementControlRoom);

		await expect.element(page.getByText('Recovery needs attention')).toBeVisible();
		await page.getByRole('button', { name: 'Mark bundle-1 as handled' }).click();

		await vi.waitFor(() => expect(h.resolveImportBundle).toHaveBeenCalledTimes(1));
		expect(h.resolveImportBundle).toHaveBeenCalledWith({ bundleId: 'bundle-1' });
		await vi.waitFor(() => expect(h.invalidate).toHaveBeenCalledOnce());
		expect(h.toast).toHaveBeenCalledWith({
			message: 'Import bundle marked as handled (4/4 files verified).',
			type: 'success'
		});
	});

	it('toasts the verification failure when resolving an import bundle fails', async () => {
		h.recovery.data = recoveryDataWith({
			needs_attention_count: 1,
			needs_attention_bundles: [{ bundle_id: 'bundle-1' }]
		});
		h.resolveImportBundle.mockRejectedValue(new Error('2 files failed verification'));
		await render(LibraryManagementControlRoom);

		await page.getByRole('button', { name: 'Mark bundle-1 as handled' }).click();

		await vi.waitFor(() => expect(h.toast).toHaveBeenCalledTimes(1));
		expect(h.toast).toHaveBeenCalledWith({
			message: '2 files failed verification',
			type: 'error'
		});
		expect(h.invalidate).not.toHaveBeenCalled();
	});

	it('hides the import bundle resolve action from non-admins', async () => {
		h.admin = false;
		h.recovery.data = recoveryDataWith({
			needs_attention_count: 1,
			needs_attention_bundles: [{ bundle_id: 'bundle-1' }]
		});
		await render(LibraryManagementControlRoom);

		await expect.element(page.getByText('Recovery needs attention')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Mark bundle-1 as handled' }))
			.not.toBeInTheDocument();
	});

	it('shows counts without resolve actions when no bundle identities are reported', async () => {
		h.recovery.data = recoveryDataWith({ needs_attention_count: 2, cleanup_pending_count: 1 });
		await render(LibraryManagementControlRoom);

		await expect.element(page.getByText('Recovery needs attention')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: /Mark .* as handled/ }))
			.not.toBeInTheDocument();
	});

	it('dismisses the recovery alert once diagnostics report nothing pending', async () => {
		h.recovery.data = recoveryDataWith();
		await render(LibraryManagementControlRoom);

		await expect.element(page.getByText('Off everywhere')).toBeVisible();
		await expect.element(page.getByText('Recovery needs attention')).not.toBeInTheDocument();
	});
});
