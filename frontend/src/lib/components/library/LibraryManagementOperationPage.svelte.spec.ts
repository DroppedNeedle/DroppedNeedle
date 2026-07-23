import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	operation: {} as Record<string, unknown>,
	results: {} as Record<string, unknown>,
	pause: vi.fn(),
	resume: vi.fn(),
	stop: vi.fn(),
	undo: vi.fn(),
	goto: vi.fn()
}));

vi.mock('$app/navigation', () => ({ goto: h.goto }));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { isAdmin: true, user: { id: 'admin-1' } }
}));
vi.mock('$lib/queries/library-management/LibraryManagementEvents', () => ({
	createLibraryManagementEvents: () => ({ start: vi.fn(), stop: vi.fn() })
}));
vi.mock('$lib/queries/library-management/LibraryManagementQueries.svelte', () => ({
	getLibraryManagementOperationQuery: () => h.operation,
	getLibraryManagementOperationResultsQuery: () => h.results
}));
vi.mock('$lib/queries/library-management/LibraryManagementMutations.svelte', () => ({
	controlLibraryManagementOperationMutation: (action: string) => ({
		mutateAsync: action === 'pause' ? h.pause : action === 'resume' ? h.resume : h.stop,
		isPending: false
	}),
	createLibraryManagementUndoPreviewMutation: () => ({ mutateAsync: h.undo, isPending: false })
}));

import LibraryManagementOperationPage from './LibraryManagementOperationPage.svelte';

function operation(overrides: Record<string, unknown> = {}): Record<string, unknown> {
	return {
		job_id: 'job-1',
		state: 'running',
		phase: 'applying',
		mode: 'apply',
		origin: 'manual',
		profile_id: 'profile-1',
		profile_name: 'Picard-style Organizer',
		profile_revision: 'profile-revision-1',
		settings_revision: 'settings-1',
		policy_revision: 'policy-1',
		catalog_revision: 1,
		proposed_settings_revision: null,
		target_root_id: null,
		selection: { kind: 'roots', ids: ['root-1'] },
		summary: {},
		created_at: 1_800_000_000,
		updated_at: 1_800_000_001,
		expires_at: null,
		expired: false,
		stale: false,
		stale_reasons: [],
		ready_for_confirmation: false,
		operation_row_revision: 12,
		operation_event_revision: 13,
		terminal_code: null,
		expected_work_count: 10,
		completed_count: 4,
		succeeded_count: 4,
		failed_count: 0,
		skipped_count: 0,
		control_request: 'none',
		undo_available_count: 0,
		undo_expired_count: 0,
		undo_expires_at: null,
		baseline_available_count: 0,
		external_refreshes: [],
		...overrides
	};
}

beforeEach(() => {
	vi.clearAllMocks();
	sessionStorage.clear();
	h.operation = { data: operation(), isLoading: false, isError: false };
	h.results = {
		data: { pages: [{ items: [], has_more: false, next_after_ordinal: null }] },
		isLoading: false,
		isError: false,
		hasNextPage: false,
		isFetchingNextPage: false,
		fetchNextPage: vi.fn()
	};
	h.pause.mockResolvedValue({});
	h.stop.mockResolvedValue({});
	h.undo.mockResolvedValue({
		job_id: 'undo-preview-1',
		preview_token: 'undo-token',
		created_at: 1,
		expires_at: 2,
		existing: false
	});
});

describe('LibraryManagementOperationPage', () => {
	it('shows truthful indeterminate progress while a preview total is still being discovered', async () => {
		h.operation = {
			data: operation({
				phase: 'planning',
				mode: 'preview',
				summary: { item_count: 1000 },
				expected_work_count: 0,
				completed_count: 0
			}),
			isLoading: false,
			isError: false
		};
		render(LibraryManagementOperationPage, { jobId: 'job-1' });

		await expect.element(page.getByText('1,000 files planned so far')).toBeVisible();
		await expect.element(page.getByText('0 / 0')).not.toBeInTheDocument();
		await expect
			.element(
				page.getByRole('progressbar', { name: 'Planning preview; 1,000 files planned so far' })
			)
			.not.toHaveAttribute('value');
	});

	it('uses the current row revision for pause and states that Stop is not rollback', async () => {
		render(LibraryManagementOperationPage, { jobId: 'job-1' });
		await page.getByRole('button', { name: 'Pause' }).click();
		expect(h.pause).toHaveBeenCalledWith({ jobId: 'job-1', expectedRevision: 12 });

		await page.getByRole('button', { name: 'Stop...' }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Stop after the current safe boundary?' }))
			.toHaveFocus();
		await expect.element(page.getByText(/Stopping keeps completed changes/)).toBeVisible();
		await expect.element(page.getByText(/does not roll them back/)).toBeVisible();
	});

	it('keeps operation Undo visibly distinct from first-management restore', async () => {
		h.operation = {
			data: operation({
				state: 'succeeded',
				phase: 'complete',
				completed_count: 10,
				succeeded_count: 9,
				undo_available_count: 9,
				undo_expires_at: 1_900_000_000,
				baseline_available_count: 9
			}),
			isLoading: false,
			isError: false
		};
		render(LibraryManagementOperationPage, { jobId: 'job-1' });

		await expect.element(page.getByRole('heading', { name: 'Undo this operation' })).toBeVisible();
		await expect
			.element(page.getByRole('heading', { name: 'First-management baseline' }))
			.toBeVisible();
		await expect
			.element(page.getByText(/9 files have an available first-management baseline/))
			.toBeVisible();
		await page.getByRole('button', { name: 'Preview Undo...' }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Generate an Undo preview?' }))
			.toHaveFocus();
		await expect.element(page.getByText('Undo is not baseline restore.')).toBeVisible();
		await expect.element(page.getByText(/9 files have an Undo snapshot/).first()).toBeVisible();
		await page.getByRole('button', { name: 'Generate Undo preview' }).click();

		expect(h.undo).toHaveBeenCalledWith({
			jobId: 'job-1',
			request: expect.objectContaining({ expected_operation_row_revision: 12 })
		});
		expect(
			sessionStorage.getItem('droppedneedle:library-management:preview-token:undo-preview-1')
		).toBe('undo-token');
		expect(h.goto).toHaveBeenCalledWith('/library/management/previews/undo-preview-1');
	});

	it('disables Undo after durable snapshots expire while keeping baseline recovery visible', async () => {
		h.operation = {
			data: operation({
				state: 'succeeded',
				phase: 'complete',
				completed_count: 2,
				succeeded_count: 2,
				undo_available_count: 0,
				undo_expired_count: 2,
				undo_expires_at: null,
				baseline_available_count: 2
			}),
			isLoading: false,
			isError: false
		};
		render(LibraryManagementOperationPage, { jobId: 'job-1' });

		await expect.element(page.getByRole('button', { name: 'Preview Undo...' })).toBeDisabled();
		await expect
			.element(
				page
					.getByRole('main')
					.getByText('Undo snapshots have expired for 2 files.', { exact: true })
			)
			.toBeVisible();
		await expect
			.element(page.getByText(/2 files have an available first-management baseline/))
			.toBeVisible();
	});

	it('shows post-commit refresh failures without implying file rollback', async () => {
		h.operation = {
			data: operation({
				state: 'succeeded',
				phase: 'complete',
				external_refreshes: [
					{
						target: 'jellyfin',
						state: 'retry_wait',
						attempts: 1,
						max_attempts: 4,
						failure_code: 'EXTERNAL_REFRESH_FAILED',
						updated_at: 1_800_000_002,
						completed_at: null
					}
				]
			}),
			isLoading: false,
			isError: false
		};
		render(LibraryManagementOperationPage, { jobId: 'job-1' });

		await expect
			.element(page.getByRole('heading', { name: 'Media-server delivery ledger' }))
			.toBeVisible();
		await expect.element(page.getByText('1 of 4 attempts used')).toBeVisible();
		await expect.element(page.getByText(/never\s+rolls those changes back/)).toBeVisible();
	});

	it('presents a clean completed terminal code as success rather than an error', async () => {
		h.operation = {
			data: operation({
				state: 'succeeded',
				phase: 'complete',
				terminal_code: 'COMPLETED',
				completed_count: 10,
				succeeded_count: 10
			}),
			isLoading: false,
			isError: false
		};
		render(LibraryManagementOperationPage, { jobId: 'job-1' });

		await expect.element(page.getByText('COMPLETED', { exact: true })).toBeVisible();
		await expect.element(page.getByText('All planned work finished.')).toBeVisible();
		await expect
			.element(page.getByText(/Recovery never silently removes an uncertain file/))
			.not.toBeInTheDocument();
	});

	it('keeps a failed terminal code visibly actionable', async () => {
		h.operation = {
			data: operation({
				state: 'failed',
				phase: 'applying',
				terminal_code: 'RECOVERY_FAILED',
				failed_count: 1
			}),
			isLoading: false,
			isError: false
		};
		render(LibraryManagementOperationPage, { jobId: 'job-1' });

		await expect.element(page.getByText('Recovery Failed')).toBeVisible();
		await expect
			.element(page.getByText(/Recovery never silently removes an uncertain file/))
			.toBeVisible();
	});

	it('keeps a failed legacy operation actionable without a terminal code', async () => {
		h.operation = {
			data: operation({ state: 'failed', terminal_code: null, failed_count: 1 }),
			isLoading: false,
			isError: false
		};
		render(LibraryManagementOperationPage, { jobId: 'job-1' });

		await expect.element(page.getByText('Operation Failed')).toBeVisible();
		await expect
			.element(page.getByText(/Recovery never silently removes an uncertain file/))
			.toBeVisible();
	});
});
