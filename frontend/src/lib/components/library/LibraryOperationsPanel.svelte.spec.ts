import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { LibraryActivityItem, ScanRun } from '$lib/queries/library/LibraryOperationsTypes';

const h = vi.hoisted(() => ({
	activity: { data: { items: [] }, isLoading: false, isError: false } as Record<string, unknown>,
	runs: { data: { active: null, queued: null }, isLoading: false, isError: false } as Record<
		string,
		unknown
	>,
	detail: { data: undefined } as Record<string, unknown>,
	operation: { data: undefined } as Record<string, unknown>,
	settings: {
		data: {
			policy_revision: 'policy-1',
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
	history: {
		data: { pages: [{ items: [], next_cursor: null }] },
		isLoading: false,
		isError: false,
		hasNextPage: false,
		isFetchingNextPage: false,
		fetchNextPage: vi.fn()
	} as Record<string, unknown>,
	repairs: { data: { pages: [{ items: [] }] }, isLoading: false } as Record<string, unknown>,
	pauseRun: vi.fn(),
	resumeRun: vi.fn(),
	stopRun: vi.fn(),
	pauseIdentification: vi.fn(),
	requestRun: vi.fn(),
	bulkPreview: vi.fn(),
	bulkApply: vi.fn(),
	toast: vi.fn()
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'admin-1' }, isAdmin: true }
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
	controlLibraryOperation: () => ({ mutateAsync: vi.fn(), isPending: false })
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
vi.mock('$lib/queries/library/LibraryReviewMutations.svelte', () => ({
	previewBulkLibraryReview: () => ({
		mutateAsync: h.bulkPreview,
		reset: vi.fn(),
		data: undefined,
		isPending: false,
		isError: false
	}),
	applyBulkLibraryReview: () => ({
		mutateAsync: h.bulkApply,
		reset: vi.fn(),
		data: undefined,
		isPending: false,
		isError: false
	})
}));
vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibraryScanScheduleQuery: () => ({
		data: { scan_frequency: 'daily', daily_scan_time: '09:00', server_timezone: 'Europe/London' }
	}),
	getLibraryStatsQuery: () => ({ data: { local_only_count: 9 } })
}));
vi.mock('$lib/queries/library/LibraryRepairQueries.svelte', () => ({
	getLibraryRepairsQuery: () => h.repairs,
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
vi.mock('./LibraryManagementControlRoom.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});

import LibraryOperationsPanel from './LibraryOperationsPanel.svelte';

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
	h.runs = { data: { active: null, queued: null }, isLoading: false, isError: false };
	h.detail = { data: undefined };
	h.operation = { data: undefined };
	h.reviews = { data: { pages: [{ filtered_total: 12 }] } };
	h.pauseRun.mockResolvedValue({});
	h.resumeRun.mockResolvedValue({});
	h.stopRun.mockResolvedValue({});
	h.pauseIdentification.mockResolvedValue({});
	h.requestRun.mockResolvedValue({});
});

describe('LibraryOperationsPanel', () => {
	it('shows separate stacked workload cards and truthful metrics', async () => {
		h.activity = {
			data: { items: [activity('scan'), activity('identification')] },
			isLoading: false,
			isError: false
		};
		h.runs = { data: { active: run(), queued: null }, isLoading: false, isError: false };
		h.detail = {
			data: {
				snapshot: {
					run: run(),
					scopes: [
						{
							root_id: 'root-1',
							scope_id: 'root-1',
							relative_path: '.',
							effective_policy: 'automatic',
							policy_revision: 'policy-1',
							estimated_count: 100
						}
					],
					counters: { changed_count: 4, unchanged_count: 36, errored_count: 1 }
				}
			}
		};
		render(LibraryOperationsPanel);
		await expect
			.element(page.getByRole('region', { name: 'Library Scanning & Identification' }))
			.toBeVisible();
		await expect.element(page.getByRole('heading', { name: 'Local files' })).toBeVisible();
		await expect
			.element(page.getByRole('heading', { name: 'Identification', exact: true }))
			.toBeVisible();
		await expect.element(page.getByText('40 of 100')).toBeVisible();
		await expect.element(page.getByText('25 of 100')).toBeVisible();
		await page.getByText('Root progress and phase details').click();
		await expect.element(page.getByText('Main library · automatic')).toBeVisible();
		await expect.element(page.getByText('12', { exact: true })).toBeVisible();
		await expect.element(page.getByText(/It never writes tags, renames, or moves/)).toBeVisible();
	});

	it('shows identification as idle while foreground repair keeps the panel expanded', async () => {
		h.activity = {
			data: {
				items: [
					activity('identification', {
						state: 'idle',
						processed: 12,
						total: 12,
						waiting_count: 0,
						identified_count: 12,
						foreground_operation_count: 1,
						priority_band: null,
						oldest_backlog_at: null
					})
				]
			},
			isLoading: false,
			isError: false
		};
		render(LibraryOperationsPanel);
		await expect.element(page.getByText('Idle').nth(1)).toBeVisible();
		await expect.element(page.getByText('12 of 12')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Pause identification' }))
			.not.toBeInTheDocument();
	});

	it('shows signed scan, queue, identification, and health details', async () => {
		h.activity = {
			data: {
				items: [
					activity('scan'),
					activity('identification', {
						priority_band: 'Administrator retries',
						oldest_backlog_at: Date.now() / 1000 - 3600,
						provider_unavailable: true
					})
				]
			},
			isLoading: false,
			isError: false
		};
		h.runs = {
			data: {
				active: run({ phase_timings: { discovering: 4 } }),
				queued: run({ id: 'run-2', kind: 'rescan_files', state: 'queued' })
			},
			isLoading: false,
			isError: false
		};
		h.detail = {
			data: {
				snapshot: {
					run: run(),
					scopes: [],
					counters: { new_count: 2, changed_count: 4, errored_count: 1 }
				}
			}
		};
		render(LibraryOperationsPanel);
		await expect.element(page.getByText(/Whole library/).first()).toBeVisible();
		await expect.element(page.getByText(/Queued follow-up: rescan files/)).toBeVisible();
		await expect.element(page.getByText('Administrator retries')).toBeVisible();
		await expect.element(page.getByText(/provider to become available/)).toBeVisible();
		await expect.element(page.getByText(/9 local-only/)).toBeVisible();
	});

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
		render(LibraryOperationsPanel);
		await expect.element(page.getByText('Pausing after the current file...')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Pause local scan' }))
			.not.toBeInTheDocument();
		await expect.element(page.getByRole('button', { name: 'Stop local scan' })).toBeVisible();
	});

	it('reports each Pause all outcome when one workload fails', async () => {
		h.activity = {
			data: { items: [activity('scan'), activity('identification')] },
			isLoading: false,
			isError: false
		};
		h.runs = {
			data: { active: run({ row_revision: 9 }), queued: null },
			isLoading: false,
			isError: false
		};
		h.pauseRun.mockResolvedValue({});
		h.pauseIdentification.mockRejectedValue(new Error('stale control revision'));
		render(LibraryOperationsPanel);
		await page.getByRole('button', { name: 'Pause all' }).click();
		expect(h.pauseRun).toHaveBeenCalledWith({ runId: 'run-1', expectedRevision: 9 });
		expect(h.pauseIdentification).toHaveBeenCalledWith(7);
		expect(h.toast).toHaveBeenCalledWith({
			message: 'local scan paused; identification needs attention',
			type: 'error'
		});
	});

	it('shows exact stop confirmation and controls the durable run', async () => {
		h.activity = { data: { items: [activity('scan')] }, isLoading: false, isError: false };
		h.runs = {
			data: { active: run({ row_revision: 11 }), queued: null },
			isLoading: false,
			isError: false
		};
		render(LibraryOperationsPanel);
		await page.getByRole('button', { name: 'Stop local scan' }).click();
		await expect.element(page.getByRole('heading', { name: 'Stop this scan?' })).toBeVisible();
		await expect.element(page.getByText(/Files already indexed will stay available/)).toBeVisible();
		await page.getByRole('button', { name: 'Stop scan' }).click();
		expect(h.stopRun).toHaveBeenCalledWith({ runId: 'run-1', expectedRevision: 11 });
	});

	it('uses approved supersession language and offers explicit policy Apply', async () => {
		h.activity = {
			data: { items: [activity('scan', { state: 'superseded_policy_changed' })] },
			isLoading: false,
			isError: false
		};
		h.runs = {
			data: {
				active: run({ state: 'superseded_policy_changed', terminal_code: 'POLICY_CHANGED' }),
				queued: null
			},
			isLoading: false,
			isError: false
		};
		render(LibraryOperationsPanel);
		await expect
			.element(page.getByText('Stopped because library policy changed').first())
			.toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Apply policy changes...' }))
			.toBeVisible();
	});

	it('opens the shared scoped retry preview with immutable policy IDs', async () => {
		h.reviews = { data: { pages: [{ filtered_total: 12, catalog_revision: 42 }] } };
		render(LibraryOperationsPanel);
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

	it('releases a finished retry so another one can be started', async () => {
		sessionStorage.setItem('droppedneedle:identification-retry:admin-1', 'job-1');
		h.operation = {
			data: {
				id: 'job-1',
				state: 'succeeded',
				expected_work_count: 12,
				completed_count: 12,
				skipped_count: 0,
				failed_count: 0,
				row_revision: 2
			}
		};
		render(LibraryOperationsPanel);
		await page.getByRole('button', { name: 'Retry identification...' }).click();
		await expect.element(page.getByText('Identification retry succeeded')).toBeVisible();
		expect(sessionStorage.getItem('droppedneedle:identification-retry:admin-1')).toBeNull();
		await page.getByRole('button', { name: 'Start another retry' }).click();
		await expect.element(page.getByRole('button', { name: 'Preview retry' })).toBeVisible();
	});
});
