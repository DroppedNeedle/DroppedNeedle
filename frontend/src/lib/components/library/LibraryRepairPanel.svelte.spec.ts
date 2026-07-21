import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { OperationResponse } from '$lib/queries/library/LibraryOperationsTypes';

const summary = {
	total_identities: 12,
	remaining_identities: 0,
	input_track_count: 120,
	playable_after_detach_track_count: 120,
	estimated_apply_changes: 3,
	catalog_snapshot_revision: 42,
	target_matcher_version: 'feedback-fixes-v1',
	counts_by_finding: {
		valid: 6,
		safe_detach: 3,
		needs_review: 1,
		unverifiable: 1,
		stale: 1,
		manual_identity: 0
	},
	counts_by_reason: { ZERO_SUPPORT: 3 },
	album_counts_by_root: { 'root-1': 12 },
	provider_deferred_count: 1,
	failed_evidence_count: 1
};

function repair(overrides: Partial<OperationResponse> = {}): OperationResponse {
	return {
		id: 'repair-1',
		kind: 'repair',
		state: 'ready',
		expected_work_count: 12,
		completed_count: 12,
		succeeded_count: 12,
		failed_count: 0,
		skipped_count: 0,
		control_request: 'none',
		terminal_code: 'DRY_RUN_READY',
		row_revision: 7,
		event_revision: 4,
		created_at: 1,
		updated_at: 2,
		results: [],
		results_truncated: false,
		repair_summary: summary,
		reidentification_candidates: [],
		...overrides
	};
}

const h = vi.hoisted(() => ({
	repairs: {
		data: { pages: [{ items: [] }] },
		isLoading: false
	} as Record<string, unknown>,
	estimate: {
		data: { identity_count: 12, selected_root_count: 0, queued_repair_count: 0 },
		isLoading: false,
		isError: false
	} as Record<string, unknown>,
	findings: {
		data: {
			pages: [
				{
					items: [
						{
							id: 'finding-1',
							local_album_id: 'album-1',
							evidence_id: 'evidence-1',
							review_id: 'review-1',
							finding_code: 'safe_detach',
							reason_code: 'ZERO_SUPPORT',
							confidence: 'complete',
							apply_eligible: true,
							state: 'open',
							apply_result: null,
							updated_at: 2,
							row_revision: 1
						}
					],
					next_cursor: null,
					has_more: false
				}
			]
		},
		isLoading: false,
		isError: false,
		hasNextPage: false,
		isFetchingNextPage: false,
		fetchNextPage: vi.fn()
	} as Record<string, unknown>,
	categoryGetter: (() => '') as () => string,
	create: vi.fn(),
	apply: vi.fn(),
	pause: vi.fn(),
	resume: vi.fn(),
	stop: vi.fn()
}));

vi.mock('$lib/queries/library/LibraryRepairQueries.svelte', () => ({
	getLibraryRepairsQuery: () => h.repairs,
	getLibraryRepairEstimateQuery: () => h.estimate,
	getLibraryRepairFindingsQuery: (_getId: () => string | null, getCategory: () => string) => {
		h.categoryGetter = getCategory;
		return h.findings;
	}
}));
vi.mock('$lib/queries/library/LibraryPolicyQueries.svelte', () => ({
	getLibraryPolicyTreeQuery: () => ({
		data: {
			roots: [
				{ id: 'root-1', label: 'Main library', available: true },
				{ id: 'root-2', label: 'Archive', available: true }
			]
		},
		isLoading: false
	})
}));
vi.mock('$lib/queries/library/LibraryRepairMutations.svelte', () => ({
	createLibraryRepair: () => ({ mutateAsync: h.create, isPending: false }),
	applyLibraryRepair: () => ({ mutateAsync: h.apply, isPending: false })
}));
vi.mock('$lib/queries/library/LibraryOperationMutations.svelte', () => ({
	controlLibraryOperation: (action: string) => ({
		mutateAsync: action === 'pause' ? h.pause : action === 'resume' ? h.resume : h.stop,
		isPending: false
	})
}));

import LibraryRepairPanel from './LibraryRepairPanel.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.repairs = { data: { pages: [{ items: [] }] }, isLoading: false };
	h.estimate = {
		data: { identity_count: 12, selected_root_count: 0, queued_repair_count: 0 },
		isLoading: false,
		isError: false
	};
	h.create.mockResolvedValue(repair({ state: 'queued', repair_summary: null }));
	h.apply.mockResolvedValue(repair({ state: 'succeeded' }));
	h.pause.mockResolvedValue(repair({ state: 'paused' }));
	h.resume.mockResolvedValue(repair({ state: 'running' }));
	h.stop.mockResolvedValue(repair({ state: 'stopped' }));
});

describe('LibraryRepairPanel', () => {
	it('previews exact scope, candidate count, and queue impact before starting', async () => {
		render(LibraryRepairPanel);
		await page.getByRole('button', { name: 'Check existing matches' }).click();
		await expect.element(page.getByText(/does not change/)).toBeVisible();
		await expect.element(page.getByText('12 identities')).toBeVisible();
		await expect.element(page.getByText(/No other repair checks are waiting/)).toBeVisible();
		await page.getByRole('radio', { name: /Selected roots/ }).click();
		await page.getByRole('checkbox', { name: 'Archive' }).click();
		await page.getByRole('button', { name: 'Start check' }).click();
		expect(h.create).toHaveBeenCalledWith(['root-1']);
	});

	it('shows the complete report and requests server-filtered tabs', async () => {
		h.repairs = { data: { pages: [{ items: [repair()] }] }, isLoading: false };
		render(LibraryRepairPanel);
		await page.getByRole('button', { name: 'View report' }).click();
		await expect
			.element(page.getByRole('tab', { name: /Safe to detach 3/ }))
			.toHaveAttribute('aria-selected', 'true');
		await expect.element(page.getByText('12', { exact: true }).first()).toBeVisible();
		await expect.element(page.getByText('120 of 120')).toBeVisible();
		await expect.element(page.getByText(/Main library: 12/)).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Open evidence' }))
			.toHaveAttribute('href', '/library/review?review=review-1');
		await page.getByRole('tab', { name: /Could not verify 2/ }).click();
		expect(h.categoryGetter()).toBe('unverifiable');
	});

	it('uses durable controls and confirms the exact safe Apply count', async () => {
		h.repairs = {
			data: { pages: [{ items: [repair({ state: 'running', row_revision: 9 })] }] },
			isLoading: false
		};
		render(LibraryRepairPanel);
		await page.getByRole('button', { name: 'Pause' }).click();
		expect(h.pause).toHaveBeenCalledWith({ jobId: 'repair-1', expectedRevision: 9 });

		h.repairs = { data: { pages: [{ items: [repair()] }] }, isLoading: false };
		render(LibraryRepairPanel);
		await page.getByRole('button', { name: 'Apply safe repairs...' }).last().click();
		await expect.element(page.getByText(/Local files, album IDs/).last()).toBeVisible();
		await expect.element(page.getByText('3 identities are eligible.')).toBeVisible();
		await page.getByRole('button', { name: 'Apply safe repairs', exact: true }).click();
		expect(h.apply).toHaveBeenCalledWith({ jobId: 'repair-1', expectedRevision: 7 });
	});

	it('shows a disabled stopping state until the worker reaches its checkpoint', async () => {
		h.repairs = {
			data: {
				pages: [
					{
						items: [repair({ state: 'running', control_request: 'stop', row_revision: 10 })]
					}
				]
			},
			isLoading: false
		};
		render(LibraryRepairPanel);
		await expect.element(page.getByText('stopping', { exact: true })).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Stopping...' })).toBeDisabled();
		await expect
			.element(page.getByRole('button', { name: 'Stop', exact: true }))
			.not.toBeInTheDocument();
	});

	it('shows three recent checks by default and keeps older audit history available', async () => {
		h.repairs = {
			data: {
				pages: [
					{
						items: [
							repair({ id: 'repair-5', completed_count: 5 }),
							repair({ id: 'repair-4', completed_count: 4 }),
							repair({ id: 'repair-3', completed_count: 3 }),
							repair({ id: 'repair-2', completed_count: 2 }),
							repair({ id: 'repair-1', completed_count: 1 })
						]
					}
				]
			},
			isLoading: false
		};
		render(LibraryRepairPanel);
		await expect.element(page.getByText('3 of 12')).toBeVisible();
		await expect.element(page.getByText('2 of 12')).not.toBeInTheDocument();
		await page.getByRole('button', { name: 'Show 2 older checks' }).click();
		await expect.element(page.getByText('2 of 12')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Show latest 3' })).toBeVisible();
	});
});
