import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { ScanRun } from '$lib/queries/library/LibraryOperationsTypes';

function run(overrides: Partial<ScanRun> = {}): ScanRun {
	return {
		id: 'run-safe-1',
		kind: 'incremental',
		trigger: 'automatic',
		state: 'completed',
		phase: 'reconciling',
		requested_by_user_id: null,
		aggregate_scope: 'root-1',
		queued_at: 100,
		started_at: 110,
		updated_at: 145,
		terminal_at: 145,
		resume_phase: null,
		requested_control: 'none',
		terminal_code: 'COMPLETED',
		coalesced_request_count: 0,
		row_revision: 2,
		event_revision: 3,
		counters: { discovered_count: 40, changed_count: 7, errored_count: 1 },
		phase_timings: { discovering: 10.5, indexing: 24.5 },
		...overrides
	};
}

const h = vi.hoisted(() => ({
	history: {
		data: { pages: [{ items: [] }] },
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
	get: vi.fn(),
	toast: vi.fn(),
	anchorClick: vi.fn()
}));

vi.mock('$lib/queries/library/LibraryOperationQueries.svelte', () => ({
	getLibraryRunHistoryQuery: () => h.history,
	getLibraryRunFailuresQuery: () => h.failures
}));
vi.mock('$lib/api/client', () => ({ api: { global: { get: h.get } } }));
vi.mock('$lib/stores/toast', () => ({ toastStore: { show: h.toast } }));

import LibraryRunHistory from './LibraryRunHistory.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.history = {
		data: { pages: [{ items: [run()] }] },
		isLoading: false,
		isError: false,
		hasNextPage: false,
		isFetchingNextPage: false,
		fetchNextPage: vi.fn()
	};
	h.failures = {
		data: { pages: [{ items: [], next_cursor: null }] },
		isLoading: false,
		isError: false,
		hasNextPage: false,
		isFetchingNextPage: false,
		fetchNextPage: vi.fn()
	};
	vi.spyOn(URL, 'createObjectURL').mockReturnValue('blob:diagnostic');
	vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => undefined);
	vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(h.anchorClick);
});

describe('LibraryRunHistory', () => {
	it('shows retained run counts, safe reason, and phase timings', async () => {
		await render(LibraryRunHistory);
		await expect.element(page.getByText('7 changed · 1 errors')).toBeVisible();
		await expect.element(page.getByRole('cell', { name: '35s' })).toBeVisible();
		await page.getByText('Details').first().click();
		await expect.element(page.getByRole('dialog', { name: 'Run details' })).toBeVisible();
		await expect.element(page.getByText('Finished normally').first()).toBeVisible();
		await expect.element(page.getByText('discovering · 10.5s').first()).toBeVisible();
		await expect.element(page.getByText('indexing · 24.5s').first()).toBeVisible();
	});

	it('confirms redaction and honors the streamed safe filename', async () => {
		h.get.mockResolvedValue(
			new Response('{"safe":true}', {
				status: 200,
				headers: {
					'content-type': 'application/json',
					'content-disposition': 'attachment; filename="droppedneedle-library-run-opaque.json"'
				}
			})
		);
		await render(LibraryRunHistory);
		await page.getByText('Details').first().click();
		await page.getByRole('button', { name: /Export diagnostics for run run-safe-1/ }).click();
		await expect
			.element(page.getByText(/excludes credentials, raw provider responses/))
			.toBeVisible();
		await page.getByRole('button', { name: 'Export report' }).click();
		expect(h.get).toHaveBeenCalledWith('/api/v1/library/scan-runs/run-safe-1/diagnostics', {
			raw: true
		});
		expect(h.anchorClick).toHaveBeenCalledOnce();
		expect(h.toast).toHaveBeenCalledWith({ message: 'Diagnostic report ready', type: 'success' });
	});

	it('uses fixed user-safe copy when an export fails', async () => {
		h.get.mockRejectedValue(new Error('/secret/path/provider-token'));
		await render(LibraryRunHistory);
		await page.getByText('Details').first().click();
		await page.getByRole('button', { name: /Export diagnostics/ }).click();
		await page.getByRole('button', { name: 'Export report' }).click();
		expect(h.toast).toHaveBeenCalledWith({
			message: 'Could not prepare the diagnostic report',
			type: 'error'
		});
		expect(document.body.textContent).not.toContain('/secret/path');
	});

	it('shows three recent runs by default and keeps older audit history available', async () => {
		h.history = {
			data: {
				pages: [
					{
						items: [
							run({ id: 'run-5', aggregate_scope: 'scope-5' }),
							run({ id: 'run-4', aggregate_scope: 'scope-4' }),
							run({ id: 'run-3', aggregate_scope: 'scope-3' }),
							run({ id: 'run-2', aggregate_scope: 'scope-2' }),
							run({ id: 'run-1', aggregate_scope: 'scope-1' })
						]
					}
				]
			},
			isLoading: false,
			isError: false,
			hasNextPage: false,
			isFetchingNextPage: false,
			fetchNextPage: vi.fn()
		};
		await render(LibraryRunHistory);

		await expect.element(page.getByText('scope-3').first()).toBeVisible();
		await expect.element(page.getByText('scope-2')).not.toBeInTheDocument();
		await page.getByRole('button', { name: 'Show 2 older runs' }).click();
		await expect.element(page.getByText('scope-2').first()).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Show latest 3' })).toBeVisible();
	});

	it('lists recorded failed paths for a timed-out run and pages with Load more', async () => {
		h.history = {
			data: {
				pages: [
					{
						items: [
							run({
								id: 'run-wedged',
								state: 'failed',
								terminal_code: 'WALK_TIMEOUT',
								counters: { discovered_count: 12, errored_count: 1 }
							})
						]
					}
				]
			},
			isLoading: false,
			isError: false,
			hasNextPage: false,
			isFetchingNextPage: false,
			fetchNextPage: vi.fn()
		};
		h.failures = {
			data: {
				pages: [
					{
						items: [
							{
								root_id: 'root-1',
								relative_path: 'Artist/Stuck Album',
								failure_code: 'WALK_TIMEOUT',
								failure_detail: 'The directory walk made no progress for 30.0s.',
								phase: 'discovering',
								recorded_at: 140
							}
						],
						next_cursor: 41
					}
				]
			},
			isLoading: false,
			isError: false,
			hasNextPage: true,
			isFetchingNextPage: false,
			fetchNextPage: vi.fn()
		};
		await render(LibraryRunHistory);

		await page.getByText('Details').first().click();
		await expect
			.element(page.getByText('Stopped because a library folder stopped responding'))
			.toBeVisible();
		await expect.element(page.getByText('Failed paths')).toBeVisible();
		await expect.element(page.getByText('Artist/Stuck Album')).toBeVisible();
		await expect.element(page.getByText('WALK_TIMEOUT').first()).toBeVisible();
		await page.getByRole('button', { name: 'Load more' }).click();
		expect(h.failures.fetchNextPage).toHaveBeenCalledOnce();
	});

	it('folds the startup_resume trigger with spaces like the state cells', async () => {
		h.history = {
			data: {
				pages: [
					{
						items: [run({ trigger: 'startup_resume' })]
					}
				]
			},
			isLoading: false,
			isError: false,
			hasNextPage: false,
			isFetchingNextPage: false,
			fetchNextPage: vi.fn()
		};
		await render(LibraryRunHistory);

		await expect.element(page.getByText('startup resume').first()).toBeVisible();
		expect(document.body.textContent).not.toContain('startup_resume');
	});

	it('omits the failed-paths section for clean completed runs', async () => {
		h.history = {
			data: {
				pages: [
					{
						items: [run({ counters: { discovered_count: 40, errored_count: 0 } })]
					}
				]
			},
			isLoading: false,
			isError: false,
			hasNextPage: false,
			isFetchingNextPage: false,
			fetchNextPage: vi.fn()
		};
		await render(LibraryRunHistory);

		await page.getByText('Details').first().click();
		await expect.element(page.getByRole('dialog', { name: 'Run details' })).toBeVisible();
		await expect.element(page.getByText('Failed paths')).not.toBeInTheDocument();
	});
});
