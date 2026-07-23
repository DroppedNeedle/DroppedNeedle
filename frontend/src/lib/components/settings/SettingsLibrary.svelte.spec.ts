import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const baseSettings = {
	library_roots: [
		{
			id: 'root-1',
			path: '/music/archive',
			label: 'Archive',
			policy: 'automatic',
			rules: []
		}
	],
	staging_path: '/staging',
	naming_template: '{albumartist}/{album}/{track} {title}',
	acoustid_api_key: '••••',
	policy_revision: 'policy-1',
	reconciliation_required: false,
	reconciliation_state: 'applied',
	pending_policy_revision: null,
	affected_scope_ids: [],
	actions_applied: [],
	warnings: []
};

const h = vi.hoisted(() => ({
	settings: { data: {}, isLoading: false, isError: false } as Record<string, unknown>,
	impactResult: {
		current_policy_revision: 'policy-1',
		proposed_policy_revision: 'policy-2',
		stale: false,
		reconciliation_required: true,
		affected_scope_ids: ['root-1'],
		indexed_file_count: 80,
		on_disk_file_count: 100,
		content_will_become_unavailable: true,
		queued_work_will_be_cancelled: false,
		warnings: []
	} as Record<string, unknown>,
	impact: vi.fn(),
	save: vi.fn(),
	applyPreview: vi.fn(),
	requestRun: vi.fn(),
	toast: vi.fn(),
	isAdmin: true
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: {
		get isAdmin() {
			return h.isAdmin;
		}
	}
}));
vi.mock('$lib/stores/toast', () => ({ toastStore: { show: h.toast } }));
vi.mock('$lib/queries/library/LibraryPolicyQueries.svelte', () => ({
	getTargetLibrarySettingsQuery: () => h.settings,
	getLibraryPolicyTreeQuery: () => ({
		data: {
			roots: [
				{
					id: 'root-1',
					label: 'Archive',
					path: '/music/archive',
					policy: 'automatic',
					available: false,
					indexed_file_count: 80,
					on_disk_file_count: null,
					children: []
				}
			]
		}
	})
}));
vi.mock('$lib/queries/library/LibraryPolicyMutations.svelte', () => ({
	previewLibraryPolicyImpact: () => ({
		mutateAsync: h.impact,
		get data() {
			return h.impactResult;
		},
		isPending: false
	}),
	saveTargetLibrarySettings: () => ({ mutateAsync: h.save, isPending: false }),
	previewLibraryPolicyApply: () => ({
		mutateAsync: h.applyPreview,
		data: {
			policy_revision: 'policy-2',
			scope_ids: ['root-1'],
			estimated_file_count: 80,
			content_will_become_unavailable: true,
			queued_work_was_cancelled_on_save: false
		},
		isPending: false
	})
}));
vi.mock('$lib/queries/library/LibraryOperationMutations.svelte', () => ({
	requestLibraryRun: () => ({ mutateAsync: h.requestRun, isPending: false })
}));
vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibraryStatsQuery: () => ({ data: { total_size_bytes: 0 } }),
	getLibraryScanScheduleQuery: () => ({
		data: { scan_frequency: 'manual', daily_scan_time: '03:00' },
		isLoading: false,
		isError: false
	})
}));
vi.mock('$lib/queries/library/LibraryMutations.svelte', () => ({
	saveLibraryScanSchedule: () => ({ mutateAsync: vi.fn(), isPending: false })
}));
vi.mock('$lib/queries/downloads/DownloadClientsQueries.svelte', () => ({
	getDownloadPolicyQuery: () => ({ data: { max_library_size_gb: 0 } }),
	saveDownloadPolicy: () => ({ mutateAsync: vi.fn(), isPending: false })
}));
import SettingsLibrary from './SettingsLibrary.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.isAdmin = true;
	h.settings = { data: structuredClone(baseSettings), isLoading: false, isError: false };
	h.impactResult = {
		current_policy_revision: 'policy-1',
		proposed_policy_revision: 'policy-2',
		stale: false,
		reconciliation_required: true,
		affected_scope_ids: ['root-1'],
		indexed_file_count: 80,
		on_disk_file_count: 100,
		content_will_become_unavailable: true,
		queued_work_will_be_cancelled: false,
		warnings: []
	};
	h.impact.mockResolvedValue(h.impactResult);
	h.save.mockResolvedValue({
		...structuredClone(baseSettings),
		policy_revision: 'policy-2',
		pending_policy_revision: 'policy-2',
		reconciliation_required: true,
		reconciliation_state: 'awaiting_reconciliation',
		affected_scope_ids: ['root-1']
	});
	h.applyPreview.mockResolvedValue({});
	h.requestRun.mockResolvedValue({});
});

describe('SettingsLibrary target policy UI', () => {
	it('keeps Library Management hidden from non-administrators', async () => {
		h.isAdmin = false;
		render(SettingsLibrary);
		await expect.element(page.getByText('Scanning & identification')).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: /Open Library Management/ }))
			.not.toBeInTheDocument();
	});

	it('sends administrators to the dedicated Library Management configuration', async () => {
		render(SettingsLibrary);
		await expect
			.element(page.getByRole('link', { name: /Open Library Management/ }))
			.toHaveAttribute('href', '/library/management#management-settings');
		await expect.element(page.getByText('Administrator workspace')).toBeVisible();
	});

	it('shows root inheritance policy, counts, path, and unavailable state', async () => {
		render(SettingsLibrary);
		await expect.element(page.getByText('Scanning & identification')).toBeVisible();
		await expect
			.element(
				page.getByText(
					'Reads files and updates DroppedNeedle. It does not change your music files.'
				)
			)
			.toBeVisible();
		await expect.element(page.getByRole('heading', { name: 'Archive' })).toBeVisible();
		await expect.element(page.getByText('/music/archive')).toBeVisible();
		await expect.element(page.getByText('Unavailable', { exact: true })).toBeVisible();
		await expect
			.element(
				page
					.getByRole('region', { name: 'Library roots' })
					.getByText(/Index files first, then try to identify albums/)
			)
			.toBeVisible();
	});

	it('previews consequences, saves without starting work, and leaves reconciliation explicit', async () => {
		render(SettingsLibrary);
		await page.getByRole('combobox').first().selectOptions('excluded');
		const opener = page.getByRole('button', { name: 'Preview and save settings' });
		await opener.click();
		expect(h.impact).toHaveBeenCalledWith(
			expect.objectContaining({ expected_policy_revision: 'policy-1' })
		);
		await expect
			.element(page.getByRole('heading', { name: 'Save library policy changes?' }))
			.toHaveFocus();
		await expect.element(page.getByText(/Some music will become unavailable/)).toBeVisible();
		await expect.element(page.getByText('Saving does not start a scan.')).toBeVisible();
		await page.getByRole('button', { name: 'Cancel' }).click();
		await expect.element(opener).toHaveFocus();
		await opener.click();
		await page.getByRole('button', { name: 'Save policies' }).click();
		expect(h.save).toHaveBeenCalled();
		expect(h.requestRun).not.toHaveBeenCalled();
		await expect.element(page.getByText('Awaiting reconciliation')).toBeVisible();
	});

	it('previews and explicitly starts reconciliation with the saved revision', async () => {
		h.settings = {
			data: {
				...structuredClone(baseSettings),
				policy_revision: 'policy-2',
				pending_policy_revision: 'policy-2',
				reconciliation_required: true,
				reconciliation_state: 'awaiting_reconciliation',
				affected_scope_ids: ['root-1']
			},
			isLoading: false,
			isError: false
		};
		render(SettingsLibrary);
		await page.getByRole('button', { name: 'Apply changes...' }).click();
		expect(h.applyPreview).toHaveBeenCalledWith({
			scope_ids: ['root-1'],
			expected_policy_revision: 'policy-2'
		});
		await expect
			.element(page.getByRole('heading', { name: 'Apply policy changes?' }))
			.toBeVisible();
		await page.getByRole('button', { name: 'Apply policy changes' }).click();
		expect(h.requestRun).toHaveBeenCalledWith({
			kind: 'policy_reconcile',
			scope_ids: ['root-1'],
			expected_policy_revision: 'policy-2'
		});
	});

	it('keeps a stale preview from saving or starting work', async () => {
		h.impactResult = { ...h.impactResult, stale: true };
		h.impact.mockResolvedValue(h.impactResult);
		render(SettingsLibrary);
		await page.getByRole('button', { name: 'Preview and save settings' }).click();
		expect(h.save).not.toHaveBeenCalled();
		expect(h.requestRun).not.toHaveBeenCalled();
		expect(h.toast).toHaveBeenCalledWith(
			expect.objectContaining({ message: expect.stringContaining('Reload this page') })
		);
	});
});
