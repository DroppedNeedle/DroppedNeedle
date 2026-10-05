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
	LAST_USER_ID_KEY: 'test:last-user',
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
	it('uses the current row revision for pause and states that Stop is not rollback', async () => {
		await render(LibraryManagementOperationPage, { jobId: 'job-1' });
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
		await render(LibraryManagementOperationPage, { jobId: 'job-1' });

		await expect.element(page.getByRole('heading', { name: 'Undo this operation' })).toBeVisible();
		await expect.element(page.getByRole('heading', { name: 'Original baseline' })).toBeVisible();
		await expect
			.element(page.getByText(/9 files have an available original baseline/))
			.toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Open baseline restore...' }))
			.toHaveAttribute('href', '/library/management?runner=baseline_restore');
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
		await render(LibraryManagementOperationPage, { jobId: 'job-1' });

		await expect.element(page.getByRole('button', { name: 'Preview Undo...' })).toBeDisabled();
		await expect
			.element(
				page
					.getByRole('main')
					.getByText('Undo snapshots have expired for 2 files.', { exact: true })
			)
			.toBeVisible();
		await expect
			.element(page.getByText(/2 files have an available original baseline/))
			.toBeVisible();
	});
});
