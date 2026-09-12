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

vi.mock('$lib/stores/authStore.svelte', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/stores/authStore.svelte')>()),
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
	it('shows the hero band with one primary action and explained secondaries', async () => {
		h.activity = {
			data: { items: [activity('scan'), activity('identification')] },
			isLoading: false,
			isError: false
		};
		h.runs = { data: { active: run(), queued: null }, isLoading: false, isError: false };
		await render(LibraryScanningPanel);
		await expect.element(page.getByRole('heading', { name: 'Scan & identify' })).toBeVisible();
		await expect.element(page.getByText(/Nothing here writes to your files/)).toBeVisible();
		expect(page.getByRole('button', { name: 'Scan for changes' }).elements()).toHaveLength(1);
		await expect.element(page.getByText(/Deep re-read of selected folders/)).toBeVisible();
		await expect.element(page.getByText(/Re-runs matching for albums still waiting/)).toBeVisible();
		await expect.element(page.getByText(/Next scan: 09:00/)).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Settings' }))
			.toHaveAttribute('href', '/settings?tab=library');
		expect(page.getByRole('button', { name: 'Pause all' }).query()).toBeNull();
		expect(page.getByRole('button', { name: 'Resume all' }).query()).toBeNull();
	});

	it('shows a disabled notice instead of scan controls when the library is off', async () => {
		h.settings = {
			data: {
				...structuredClone(h.settings.data as Record<string, unknown>),
				enabled: false
			},
			isSuccess: true,
			isLoading: false
		};
		await render(LibraryScanningPanel);
		await expect.element(page.getByText('The local library is disabled')).toBeVisible();
		expect(page.getByRole('heading', { name: 'Scan & identify' }).query()).toBeNull();
		expect(page.getByRole('button', { name: 'Scan for changes' }).query()).toBeNull();
		expect(page.getByRole('button', { name: 'Rescan files...' }).query()).toBeNull();
		expect(page.getByRole('button', { name: 'Retry identification...' }).query()).toBeNull();
	});

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
		await render(LibraryScanningPanel);
		await expect.element(page.getByRole('heading', { name: 'Local files' })).toBeVisible();
		await expect
			.element(page.getByRole('heading', { name: 'Identification', exact: true }))
			.toBeVisible();
		await expect.element(page.getByText('40 of 100')).toBeVisible();
		await expect.element(page.getByText('25 of 100')).toBeVisible();
		await page.getByText('Root progress and phase details').click();
		await expect.element(page.getByText('Main library · automatic')).toBeVisible();
	});

	it('shows identification as idle while foreground repair work runs', async () => {
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
		await render(LibraryScanningPanel);
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
		await render(LibraryScanningPanel);
		await expect.element(page.getByText(/Whole library/).first()).toBeVisible();
		await expect.element(page.getByText(/Queued follow-up: rescan files/)).toBeVisible();
		await expect.element(page.getByText('Administrator retries')).toBeVisible();
		await expect.element(page.getByText(/MusicBrainz is currently unavailable/)).toBeVisible();
	});

	it('surfaces an error alert when albums need attention, ahead of other tiers', async () => {
		h.activity = {
			data: {
				items: [
					activity('identification', {
						attention_count: 3,
						provider_unavailable: true,
						deferred_count: 4,
						deferred_reason_counts: { PROVIDER_TEMPORARILY_UNAVAILABLE: 4 }
					})
				]
			},
			isLoading: false,
			isError: false
		};
		await render(LibraryScanningPanel);
		await expect.element(page.getByText(/3\s+albums need attention/)).toBeVisible();
		expect(page.getByText(/MusicBrainz is currently unavailable/).query()).toBeNull();
		expect(page.getByText(/retry automatically/).query()).toBeNull();
	});

	it('explains deferred work with a reason breakdown when the provider is healthy', async () => {
		h.activity = {
			data: {
				items: [
					activity('identification', {
						deferred_count: 3,
						deferred_reason_counts: {
							PROVIDER_TEMPORARILY_UNAVAILABLE: 2,
							SUBJECT_NOT_AVAILABLE: 1
						}
					})
				]
			},
			isLoading: false,
			isError: false
		};
		await render(LibraryScanningPanel);
		await expect.element(page.getByText(/3\s+metadata\s+checks are deferred/)).toBeVisible();
		await expect.element(page.getByText(/provider temporarily unavailable: 2/)).toBeVisible();
		await expect.element(page.getByText(/album no longer available: 1/)).toBeVisible();
		expect(page.getByText(/need attention/).query()).toBeNull();
		expect(page.getByText(/MusicBrainz is currently unavailable/).query()).toBeNull();
	});

	it('lists deferred album summaries inside the deferred alert', async () => {
		h.activity = {
			data: {
				items: [
					activity('identification', {
						deferred_count: 2,
						deferred_reason_counts: { UNEXPECTED_ERROR: 2 },
						deferred_jobs: [
							{
								job_id: 'job-1',
								local_album_id: 'album-1',
								album_title: 'Stuck Album',
								artist_name: 'Stuck Artist',
								last_failure_code: 'UNEXPECTED_ERROR',
								attempt_count: 3,
								not_before: 100,
								updated_at: 90
							},
							{
								job_id: 'job-2',
								local_album_id: null,
								album_title: null,
								artist_name: null,
								last_failure_code: 'UNEXPECTED_ERROR',
								attempt_count: 1,
								not_before: null,
								updated_at: 80
							}
						]
					})
				]
			},
			isLoading: false,
			isError: false
		};
		await render(LibraryScanningPanel);
		await expect.element(page.getByText('Stuck Album')).toBeVisible();
		await expect.element(page.getByText(/- Stuck Artist/)).toBeVisible();
		await expect.element(page.getByText(/unexpected error · attempt 3/)).toBeVisible();
		await expect.element(page.getByText('Track-level work')).toBeVisible();
	});

	it('labels unmappable provider payloads honestly without an outage warning', async () => {
		h.activity = {
			data: {
				items: [
					activity('identification', {
						deferred_count: 1,
						deferred_reason_counts: { UNMAPPABLE_PROVIDER_PAYLOAD: 1 },
						provider_unavailable: false
					})
				]
			},
			isLoading: false,
			isError: false
		};
		await render(LibraryScanningPanel);
		await expect.element(page.getByText(/1\s+metadata\s+check is deferred/)).toBeVisible();
		await expect
			.element(
				page.getByText(/provider response could not be mapped \(data problem, not an outage\): 1/)
			)
			.toBeVisible();
		expect(page.getByText(/MusicBrainz is currently unavailable/).query()).toBeNull();
	});

	it('shows no identification alert when nothing is deferred or needs attention', async () => {
		h.activity = {
			data: {
				items: [activity('identification', { deferred_count: 0 })]
			},
			isLoading: false,
			isError: false
		};
		await render(LibraryScanningPanel);
		await expect
			.element(page.getByRole('heading', { name: 'Identification', exact: true }))
			.toBeVisible();
		expect(page.getByText(/need attention/).query()).toBeNull();
		expect(page.getByText(/MusicBrainz is currently unavailable/).query()).toBeNull();
		expect(page.getByText(/retry automatically/).query()).toBeNull();
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
		await render(LibraryScanningPanel);
		await expect
			.element(page.getByText('Stopped because library policy changed').first())
			.toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Apply policy changes...' }))
			.toBeVisible();
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
		await render(LibraryScanningPanel);
		await page.getByRole('button', { name: 'Retry identification...' }).click();
		await expect.element(page.getByText('Identification retry succeeded')).toBeVisible();
		expect(sessionStorage.getItem('droppedneedle:identification-retry:admin-1')).toBeNull();
		await page.getByRole('button', { name: 'Start another retry' }).click();
		await expect.element(page.getByRole('button', { name: 'Preview retry' })).toBeVisible();
	});

	it('names the file watcher when the rolling schedule is manual', async () => {
		h.schedule = {
			data: { scan_frequency: 'manual', daily_scan_time: '03:00', server_timezone: '' }
		};
		await render(LibraryScanningPanel);
		await expect
			.element(page.getByText('Scheduled scans off (file watcher still active)'))
			.toBeVisible();
	});
});
