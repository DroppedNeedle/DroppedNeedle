import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { LibraryWorkItem } from '$lib/queries/library/LibraryOperationsTypes';

const h = vi.hoisted(() => ({
	activity: {
		data: { work_items: [] as LibraryWorkItem[] },
		isLoading: false,
		isError: false
	} as Record<string, unknown>,
	history: {
		data: { pages: [{ items: [] as Array<Record<string, unknown>>, next_cursor: null }] },
		isLoading: false,
		isError: false
	} as Record<string, unknown>,
	settings: {
		data: {
			policy_revision: 'policy-1',
			enabled: true,
			library_roots: [],
			affected_scope_ids: []
		},
		isLoading: false,
		isError: false
	} as Record<string, unknown>,
	reviews: { data: { pages: [{ filtered_total: 12, catalog_revision: 3 }] } } as Record<
		string,
		unknown
	>,
	stats: {
		data: {
			total_albums: 40,
			total_artists: 18,
			total_tracks: 1234,
			total_size_bytes: 0,
			format_breakdown: {},
			review_count: 12,
			local_only_count: 9,
			last_scan_at: null
		}
	} as Record<string, unknown>,
	schedule: {
		data: { scan_frequency: 'daily', daily_scan_time: '09:00', server_timezone: 'Europe/London' }
	} as Record<string, unknown>,
	operations: {
		data: { pages: [{ items: [] as Array<Record<string, unknown>> }] },
		isLoading: false,
		isError: false
	} as Record<string, unknown>,
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
	} as Record<string, unknown>,
	identityEstimate: {
		data: {
			album_count: 12,
			ready_album_count: 4,
			mapping_required_count: 0,
			exact_release_required_count: 0,
			selected_root_count: 0,
			queued_preparation_count: 0
		},
		isLoading: false,
		isError: false
	} as Record<string, unknown>,
	requestRun: vi.fn()
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'admin-1' }, isAdmin: true }
}));
vi.mock('$lib/queries/library/LibraryActivityQueries.svelte', () => ({
	getLibraryActivityQuery: () => h.activity
}));
vi.mock('$lib/queries/library/LibraryOperationQueries.svelte', () => ({
	getLibraryRunHistoryQuery: () => h.history
}));
vi.mock('$lib/queries/library/LibraryOperationMutations.svelte', () => ({
	requestLibraryRun: () => ({ mutateAsync: h.requestRun, isPending: false })
}));
vi.mock('$lib/queries/library/LibraryPolicyQueries.svelte', () => ({
	getTargetLibrarySettingsQuery: () => h.settings
}));
vi.mock('$lib/queries/library/LibraryReviewQueries.svelte', () => ({
	getLibraryReviewsQuery: () => h.reviews
}));
vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibraryStatsQuery: () => h.stats,
	getLibraryScanScheduleQuery: () => h.schedule
}));
vi.mock('$lib/queries/library-management/LibraryManagementQueries.svelte', () => ({
	getLibraryManagementOperationsQuery: () => h.operations,
	getLibraryManagementRecoveryQuery: () => h.recovery
}));
vi.mock('$lib/queries/library/LibraryIdentityPreparationQueries.svelte', () => ({
	getLibraryIdentityPreparationEstimateQuery: () => h.identityEstimate
}));

import LibraryOverviewPanel from './LibraryOverviewPanel.svelte';

function workItem(overrides: Partial<LibraryWorkItem> = {}): LibraryWorkItem {
	return {
		id: 'work-1',
		kind: 'scan',
		state: 'running',
		phase: 'indexing',
		mode: null,
		effect: 'catalog_only',
		processed: 40,
		total: 100,
		unit: 'files',
		indeterminate: false,
		remaining_count: null,
		subject_count: null,
		started_at: 1,
		updated_at: 2,
		origin: null,
		profile_name: null,
		scope_label: null,
		new_count: 0,
		changed_count: 0,
		missing_count: 0,
		warning_count: 0,
		blocked_count: 0,
		succeeded_count: 0,
		failed_count: 0,
		skipped_count: 0,
		priority: 0,
		failure_event_id: null,
		failure_at: null,
		...overrides
	};
}

beforeEach(() => {
	vi.clearAllMocks();
	h.activity = { data: { work_items: [] }, isLoading: false, isError: false };
	h.recovery = {
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
	};
	h.operations = { data: { pages: [{ items: [] }] }, isLoading: false, isError: false };
	h.schedule = {
		data: { scan_frequency: 'daily', daily_scan_time: '09:00', server_timezone: 'Europe/London' }
	};
	h.requestRun.mockResolvedValue({});
});

describe('LibraryOverviewPanel', () => {
	it('shows an idle Current Work hero when nothing is running', async () => {
		await render(LibraryOverviewPanel);
		await expect.element(page.getByText('Nothing is running right now')).toBeVisible();
		await expect.element(page.getByText(/anything in progress will show up/)).toBeVisible();
	});

	it('shows active scan work with progress and an Open details link', async () => {
		h.activity = { data: { work_items: [workItem()] }, isLoading: false, isError: false };
		await render(LibraryOverviewPanel);
		await expect.element(page.getByRole('heading', { name: 'Scanning library' })).toBeVisible();
		await expect.element(page.getByText(/40 \/ 100 files/)).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Open details' }))
			.toHaveAttribute('href', '/library/management?tab=scanning');
	});

	it('shows track and review stats with a link to the review queue', async () => {
		await render(LibraryOverviewPanel);
		await expect.element(page.getByText('1,234')).toBeVisible();
		await expect.element(page.getByText('Needs review')).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: /Tracks/ }))
			.toHaveAttribute('href', '/library/tracks');
		await expect
			.element(page.getByRole('link', { name: /Last scan/ }))
			.toHaveAttribute('href', '/library/management?tab=scanning#recent-runs');
		await expect
			.element(page.getByRole('link', { name: /Needs review/ }))
			.toHaveAttribute('href', '/library/review');
	});

	it('replaces the previews tile with Needs attention when recovery needs work', async () => {
		h.recovery = {
			data: {
				recoverable_bundle_count: 0,
				nonterminal_journal_count: 0,
				needs_attention_count: 2,
				cleanup_pending_count: 1,
				oldest_updated_at: null,
				state_counts: {}
			},
			isLoading: false,
			isError: false
		};
		await render(LibraryOverviewPanel);
		await expect.element(page.getByText('Needs attention')).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: /Needs attention/ }))
			.toHaveAttribute('href', '/library/management?tab=organize');
		expect(page.getByText('Ready previews').elements()).toHaveLength(0);
	});

	it('requests an incremental scan from the quick action', async () => {
		await render(LibraryOverviewPanel);
		await page.getByRole('button', { name: 'Scan for changes' }).click();
		expect(h.requestRun).toHaveBeenCalledWith({
			kind: 'incremental',
			scope_ids: [],
			expected_policy_revision: 'policy-1'
		});
	});

	it('shows a disabled notice and blocks the scan action when the library is off', async () => {
		h.settings = {
			data: { ...(h.settings.data as Record<string, unknown>), enabled: false },
			isLoading: false,
			isError: false
		};
		await render(LibraryOverviewPanel);
		await expect.element(page.getByText('The local library is disabled')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Scan for changes' })).toBeDisabled();
	});

	it('names the file watcher when the rolling schedule is manual', async () => {
		h.schedule = {
			data: { scan_frequency: 'manual', daily_scan_time: '03:00', server_timezone: '' }
		};
		await render(LibraryOverviewPanel);
		await expect
			.element(page.getByText('Scheduled scans off (file watcher still active)'))
			.toBeVisible();
	});

	it('shows a waiting-for-scan hint on queued previews while a scan is active', async () => {
		h.activity = {
			data: {
				work_items: [
					workItem(),
					workItem({
						id: 'preview-1',
						kind: 'library_management',
						state: 'queued',
						phase: 'planning',
						mode: 'preview',
						effect: 'catalog_only',
						processed: 0,
						total: null,
						unit: 'items',
						indeterminate: true,
						remaining_count: null,
						subject_count: null,
						started_at: null
					})
				]
			},
			isLoading: false,
			isError: false
		};
		await render(LibraryOverviewPanel);
		await expect.element(page.getByText('Preparing a Picard-style preview')).toBeVisible();
		await expect.element(page.getByText(/Waiting for scan/)).toBeVisible();
	});

	it('shows no waiting-for-scan hint on queued previews when no scan is active', async () => {
		h.activity = {
			data: {
				work_items: [
					workItem({
						id: 'preview-1',
						kind: 'library_management',
						state: 'queued',
						phase: 'planning',
						mode: 'preview',
						effect: 'catalog_only',
						processed: 0,
						total: null,
						unit: 'items',
						indeterminate: true,
						remaining_count: null,
						subject_count: null,
						started_at: null
					})
				]
			},
			isLoading: false,
			isError: false
		};
		await render(LibraryOverviewPanel);
		await expect.element(page.getByText('Preparing a Picard-style preview')).toBeVisible();
		expect(page.getByText(/Waiting for scan/).elements()).toHaveLength(0);
	});
});
