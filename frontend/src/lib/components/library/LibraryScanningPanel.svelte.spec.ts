import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { LibraryActivityItem, ScanRun } from '$lib/queries/library/LibraryOperationsTypes';

const h = vi.hoisted(() => ({
	activity: { data: { items: [], work_items: [] }, isLoading: false, isError: false } as Record<
		string,
		unknown
	>,
	runs: { data: { active: null, queued: null }, isLoading: false, isError: false } as Record<
		string,
		unknown
	>,
	schedule: {
		data: { scan_frequency: 'daily', daily_scan_time: '09:00', server_timezone: 'Europe/London' }
	} as Record<string, unknown>,
	detail: { data: undefined } as Record<string, unknown>,
	operation: { data: undefined } as Record<string, unknown>,
	settings: {
		data: {
			policy_revision: 'policy-1',
			enabled: true,
			library_roots: [
				{ id: 'root-1', label: 'Main library', path: '/music', policy: 'automatic', rules: [] }
			],
			affected_scope_ids: [],
			reconciliation_required: false
		},
		isSuccess: true,
		isLoading: false
	} as Record<string, unknown>,
	reviews: { data: { pages: [{ filtered_total: 12 }] } } as Record<string, unknown>,
	artistReconciliation: {
		data: {
			automatically_resolved_count: 14,
			waiting_for_identity_count: 49,
			genuine_review_count: 3
		},
		isLoading: false,
		isError: false
	} as Record<string, unknown>,
	history: {
		data: { pages: [{ items: [], next_cursor: null }] },
		isLoading: false,
		isError: false,
		hasNextPage: false,
		isFetchingNextPage: false,
		fetchNextPage: vi.fn()
	} as Record<string, unknown>,
	failures: {
		data: { pages: [{ items: [], next_cursor: null }] },
		isLoading: false,
		isError: false,
		hasNextPage: false,
		isFetchingNextPage: false,
		fetchNextPage: vi.fn()
	} as Record<string, unknown>,
	pauseRun: vi.fn(),
	resumeRun: vi.fn(),
	stopRun: vi.fn(),
	pauseIdentification: vi.fn(),
	requestRun: vi.fn(),
	bulkPreview: vi.fn(),
	bulkApply: vi.fn(),
	bulkPreviewReset: vi.fn(),
	bulkApplyReset: vi.fn(),
	controlOperationMutate: vi.fn(),
	toast: vi.fn()
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'admin-1' }, isAdmin: true },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));
vi.mock('$lib/stores/toast', () => ({ toastStore: { show: h.toast } }));
vi.mock('$lib/queries/library/LibraryActivityQueries.svelte', () => ({
	getLibraryActivityQuery: () => h.activity
}));
vi.mock('$lib/queries/library/LibraryOperationQueries.svelte', () => ({
	getCurrentLibraryRunsQuery: () => h.runs,
	getLibraryRunQuery: () => h.detail,
	getLibraryOperationQuery: (getId: () => string | null) => ({
		get data() {
			return getId() ? h.operation.data : undefined;
		}
	}),
	getLibraryRunHistoryQuery: () => h.history,
	getLibraryRunFailuresQuery: () => h.failures,
	getLibraryRunEstimateQuery: () => ({ data: { estimated_file_count: 100 }, isFetching: false })
}));
vi.mock('$lib/queries/library/LibraryOperationMutations.svelte', () => ({
	requestLibraryRun: () => ({ mutateAsync: h.requestRun, isPending: false }),
	controlLibraryRun: (action: string) => ({
		mutateAsync: action === 'pause' ? h.pauseRun : action === 'resume' ? h.resumeRun : h.stopRun,
		isPending: false
	}),
	controlIdentification: (action: string) => ({
		mutateAsync: action === 'pause' ? h.pauseIdentification : h.resumeRun,
		isPending: false
	}),
	controlLibraryOperation: () => ({ mutateAsync: h.controlOperationMutate, isPending: false })
}));
vi.mock('$lib/queries/library/LibraryPolicyQueries.svelte', () => ({
	getTargetLibrarySettingsQuery: () => h.settings,
	getLibraryPolicyTreeQuery: () => ({
		data: {
			policy_revision: 'policy-1',
			roots: [
				{
					id: 'root-1',
					label: 'Main library',
					path: '/music',
					policy: 'automatic',
					available: true,
					children: [
						{
							id: 'rule-local',
							kind: 'rule',
							label: 'Bootlegs',
							path: 'Bootlegs',
							policy: 'local_metadata',
							inherited_from_id: 'rule-local',
							available: true,
							indexed_file_count: 4,
							on_disk_file_count: 4,
							children: []
						}
					]
				}
			]
		},
		isSuccess: true,
		isLoading: false,
		isError: false
	})
}));
vi.mock('$lib/queries/library/LibraryReviewQueries.svelte', () => ({
	getLibraryReviewsQuery: () => h.reviews
}));
vi.mock('$lib/queries/artist-reconciliation/ArtistReconciliationQueries.svelte', () => ({
	getArtistReconciliationProgressQuery: () => h.artistReconciliation
}));
vi.mock('$lib/queries/library/LibraryReviewMutations.svelte', () => ({
	previewBulkLibraryReview: () => ({
		mutateAsync: h.bulkPreview,
		reset: h.bulkPreviewReset,
		data: undefined,
		isPending: false,
		isError: false
	}),
	applyBulkLibraryReview: () => ({
		mutateAsync: h.bulkApply,
		reset: h.bulkApplyReset,
		data: undefined,
		isPending: false,
		isError: false
	})
}));
vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibraryScanScheduleQuery: () => h.schedule,
	getLibraryStatsQuery: () => ({ data: { local_only_count: 9 } })
}));
import LibraryScanningPanel from './LibraryScanningPanel.svelte';

function activity(
	kind: 'scan' | 'identification',
	overrides: Partial<LibraryActivityItem> = {}
): LibraryActivityItem {
	return {
		kind,
		state: 'running',
		label: kind,
		processed: kind === 'scan' ? 40 : 25,
		total: 100,
		indeterminate: false,
		updated_at: 10,
		started_at: 1,
		waiting_count: kind === 'identification' ? 75 : 0,
		identified_count: kind === 'identification' ? 20 : 0,
		kept_local_count: kind === 'identification' ? 3 : 0,
		needs_review_count: kind === 'identification' ? 5 : 0,
		failed_count: 0,
		deferred_count: 2,
		deferred_reason_counts: {},
		deferred_jobs: [],
		attention_count: 0,
		priority_band: kind === 'identification' ? 'New and changed albums' : null,
		oldest_backlog_at: kind === 'identification' ? 1 : null,
		provider_unavailable: false,
		control_revision: kind === 'identification' ? 7 : null,
		failure_event_id: null,
		failure_at: null,
		foreground_operation_count: 0,
		...overrides
	};
}

function run(overrides: Partial<ScanRun> = {}): ScanRun {
	return {
		id: 'run-1',
		kind: 'incremental',
		trigger: 'manual',
		state: 'indexing',
		phase: 'indexing',
		requested_by_user_id: 'admin-1',
		aggregate_scope: 'all',
		queued_at: 1,
		started_at: 2,
		updated_at: 3,
		terminal_at: null,
		resume_phase: null,
		requested_control: 'none',
		terminal_code: null,
		coalesced_request_count: 0,
		row_revision: 4,
		event_revision: 5,
		counters: {},
		phase_timings: {},
		...overrides
	};
}

beforeEach(() => {
	vi.clearAllMocks();
	sessionStorage.clear();
	h.activity = { data: { items: [] }, isLoading: false, isError: false };
	h.settings = {
		data: {
			policy_revision: 'policy-1',
			enabled: true,
			library_roots: [
				{ id: 'root-1', label: 'Main library', path: '/music', policy: 'automatic', rules: [] }
			],
			affected_scope_ids: [],
			reconciliation_required: false
		},
		isSuccess: true,
		isLoading: false
	};
	h.runs = { data: { active: null, queued: null }, isLoading: false, isError: false };
	h.schedule = {
		data: { scan_frequency: 'daily', daily_scan_time: '09:00', server_timezone: 'Europe/London' }
	};
	h.detail = { data: undefined };
	h.operation = { data: undefined };
	h.reviews = { data: { pages: [{ filtered_total: 12 }] } };
	h.pauseRun.mockResolvedValue({});
	h.resumeRun.mockResolvedValue({});
	h.stopRun.mockResolvedValue({});
	h.pauseIdentification.mockResolvedValue({});
	h.requestRun.mockResolvedValue({});
});

describe('LibraryScanningPanel', () => {
	it('projects a persisted pausing state and sends the current revision', async () => {
		h.activity = {
			data: { items: [activity('scan', { state: 'pausing' })] },
			isLoading: false,
			isError: false
		};
		h.runs = {
			data: { active: run({ state: 'pausing', row_revision: 9 }), queued: null },
			isLoading: false,
			isError: false
		};
		await render(LibraryScanningPanel);
		await expect.element(page.getByText('Pausing after the current file...')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Pause local scan' }))
			.not.toBeInTheDocument();
		await expect.element(page.getByRole('button', { name: 'Stop local scan' })).toBeVisible();
	});

	it('shows exact stop confirmation and controls the durable run', async () => {
		h.activity = { data: { items: [activity('scan')] }, isLoading: false, isError: false };
		h.runs = {
			data: { active: run({ row_revision: 11 }), queued: null },
			isLoading: false,
			isError: false
		};
		await render(LibraryScanningPanel);
		await page.getByRole('button', { name: 'Stop local scan' }).click();
		await expect.element(page.getByRole('heading', { name: 'Stop this scan?' })).toBeVisible();
		await expect.element(page.getByText(/Files already indexed will stay available/)).toBeVisible();
		await page.getByRole('button', { name: 'Stop scan' }).click();
		expect(h.stopRun).toHaveBeenCalledWith({ runId: 'run-1', expectedRevision: 11 });
	});

	it('opens the shared scoped retry preview with immutable policy IDs', async () => {
		h.reviews = { data: { pages: [{ filtered_total: 12, catalog_revision: 42 }] } };
		await render(LibraryScanningPanel);
		await page.getByRole('button', { name: 'Retry identification...' }).click();
		await expect.element(page.getByRole('heading', { name: 'Retry identification' })).toBeVisible();
		await page.getByRole('checkbox').nth(1).click();
		await expect.element(page.getByText(/one-off external identification action/)).toBeVisible();
		await page.getByRole('button', { name: 'Preview retry' }).click();
		expect(h.bulkPreview).toHaveBeenCalledWith({
			action: 'retry',
			selection: {
				review_ids: [],
				expected_revisions: {},
				normalized_filter: {
					states: JSON.stringify(['needs_review', 'keep_tagged']),
					scope_revision: 'policy-1',
					scope_ids: JSON.stringify(['rule-local'])
				},
				catalog_revision: 42
			}
		});
	});
});
