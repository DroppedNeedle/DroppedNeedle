import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import { authStore } from '$lib/stores/authStore.svelte';
import {
	h,
	profileId,
	baseSettings,
	roots,
	activeAssignmentSettings,
	adminUser,
	resetLibraryManagementMocks
} from './libraryManagementTestFixtures';

vi.mock(
	'$lib/queries/library-management/LibraryManagementPreviewTokens',
	async () => (await import('./libraryManagementTestFixtures')).previewTokensMock
);
vi.mock(
	'$lib/queries/library-management/LibraryManagementQueries.svelte',
	async () => (await import('./libraryManagementTestFixtures')).queriesMock
);
vi.mock(
	'$lib/queries/library-management/LibraryManagementMutations.svelte',
	async () => (await import('./libraryManagementTestFixtures')).mutationsMock
);

import SettingsLibraryManagement from './SettingsLibraryManagement.svelte';

beforeEach(resetLibraryManagementMocks);

describe('LibraryManagementActivationDialog', () => {
	it('keeps activation sessions isolated when the authenticated admin changes', async () => {
		const response = baseSettings();
		const { settings_revision: _settingsRevision, ...savedSettings } = response;
		const activationSettings = structuredClone(savedSettings);
		activationSettings.root_assignments = [
			{
				root_id: 'root-1',
				profile_id: profileId,
				overrides: null,
				enabled: true,
				automatic_acquisitions: true,
				automatic_drop_imports: false,
				automatic_scan_discovered: false,
				automatic_custom_editions: false,
				activation_profile_revision: null,
				activation_naming_policy_revision: null,
				activation_policy_revision: null,
				activation_settings_revision: null,
				activation_preview_token: null,
				activation_preview_hash: null,
				activation_confirmed_at: null,
				activation_confirmed_by: null
			}
		];
		h.readSession.mockImplementation((userId: string) =>
			userId === 'admin-1'
				? {
						sourceRevision: 'settings-1',
						policyRevision: 'policy-1',
						draft: activationSettings,
						activationDraft: activationSettings,
						rootIds: ['root-1'],
						rootIndex: 0,
						jobId: 'preview-1',
						previewToken: 'token-1',
						proofs: []
					}
				: null
		);

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await vi.waitFor(() => expect(h.readSession).toHaveBeenCalledWith('admin-1'));
		h.rememberSession.mockClear();

		authStore.setUser(adminUser('admin-2'));

		await vi.waitFor(() => expect(h.readSession).toHaveBeenCalledWith('admin-2'));
		await vi.waitFor(() => expect(h.forgetSession).toHaveBeenCalledWith('admin-2'));
		expect(h.rememberSession).not.toHaveBeenCalledWith(
			'admin-2',
			expect.objectContaining({ previewToken: 'token-1' })
		);
	});

	it('requires a current dry run and exact phrase before first automatic activation', async () => {
		h.impact.mockResolvedValue({
			current_settings_revision: 'settings-1',
			proposed_settings_revision: 'settings-2',
			stale: false,
			classification: 'destructive',
			preview_required: true,
			affected_root_ids: ['root-1'],
			reasons: ['automatic trigger enabled']
		});
		const activationData = {
			job_id: 'preview-1',
			state: 'ready',
			ready_for_confirmation: true,
			expired: false,
			stale: false,
			summary: {
				eligible_count: 8,
				warning_count: 1,
				blocked_count: 0,
				path_change_count: 7
			}
		};
		h.activation = {
			data: activationData,
			isLoading: false,
			refetch: vi.fn(async () => ({ data: activationData, error: null }))
		};

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();
		await page.getByRole('checkbox', { name: /Acquisitions/ }).click();
		await page.getByRole('button', { name: 'Validate and save' }).click();

		expect(h.update).not.toHaveBeenCalled();
		await expect
			.element(page.getByRole('heading', { name: 'Enable file organization' }))
			.toHaveFocus();
		await page.getByRole('button', { name: 'Run dry run' }).click();
		await expect.element(page.getByText('Eligible').first()).toBeVisible();
		expect(h.remember).toHaveBeenCalledWith('preview-1', 'token-1');
		await expect
			.element(page.getByRole('link', { name: 'Review file-by-file dry run' }))
			.toHaveAttribute('href', '/library/management/previews/preview-1');
		await page.getByRole('button', { name: 'Use this dry run' }).click();

		const enableButton = page.getByRole('button', { name: 'Enable file organization' });
		await expect.element(enableButton).toBeDisabled();
		await page.getByRole('textbox', { name: /Type CONFIRM/ }).fill('Enable file organization');
		await expect.element(enableButton).toBeDisabled();
		await page.getByRole('textbox', { name: /Type CONFIRM/ }).fill('CONFIRM');
		await expect.element(enableButton).toBeEnabled();
		await enableButton.click();
		expect(h.confirmActivation).toHaveBeenCalledWith(
			expect.objectContaining({
				confirmation: true,
				proofs: [{ root_id: 'root-1', job_id: 'preview-1', preview_token: 'token-1' }]
			})
		);
	});

	it('offers a fresh dry run when a saved activation goes stale', async () => {
		const settings = activeAssignmentSettings();
		h.settings = { data: settings, isLoading: false, isError: false, refetch: vi.fn() };
		h.health = {
			data: { stale_root_ids: ['root-1'], blocked_root_ids: [] },
			isLoading: false,
			isFetching: false
		};

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });

		await expect.element(page.getByText('Activation stale: run a dry run')).toBeVisible();
		await expect.element(page.getByText('Automatic writes paused: dry run needed')).toBeVisible();
		await expect
			.element(page.getByText(/Archive no longer matches its confirmed dry run/))
			.toBeVisible();
		await page.getByRole('button', { name: 'Run a fresh dry run' }).click();

		await expect
			.element(page.getByRole('heading', { name: 'Enable file organization' }))
			.toHaveFocus();
		await page.getByRole('button', { name: 'Run dry run' }).click();
		expect(h.createActivation).toHaveBeenCalledWith(expect.objectContaining({ root_id: 'root-1' }));
	});

	it('rechecks a ready dry run before accepting it for activation', async () => {
		h.impact.mockResolvedValue({
			current_settings_revision: 'settings-1',
			proposed_settings_revision: 'settings-2',
			stale: false,
			classification: 'destructive',
			preview_required: true,
			affected_root_ids: ['root-1'],
			reasons: ['automatic trigger enabled']
		});
		const ready = {
			job_id: 'preview-1',
			state: 'ready',
			ready_for_confirmation: true,
			expired: false,
			stale: false,
			summary: {
				eligible_count: 8,
				warning_count: 1,
				blocked_count: 0,
				path_change_count: 7
			}
		};
		const refetch = vi.fn(async () => ({
			data: { ...ready, ready_for_confirmation: false, stale: true },
			error: null
		}));
		h.activation = { data: ready, isLoading: false, refetch };

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();
		await page.getByRole('checkbox', { name: /Acquisitions/ }).click();
		await page.getByRole('button', { name: 'Validate and save' }).click();
		await page.getByRole('button', { name: 'Run dry run' }).click();
		await page.getByRole('button', { name: 'Use this dry run' }).click();

		expect(refetch).toHaveBeenCalledOnce();
		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent('This dry run is no longer current.');
		await expect
			.element(page.getByRole('textbox', { name: /Type CONFIRM/ }))
			.not.toBeInTheDocument();
	});

	it('cannot authorize settings edited after a dry run started', async () => {
		h.impact.mockResolvedValue({
			current_settings_revision: 'settings-1',
			proposed_settings_revision: 'settings-2',
			stale: false,
			classification: 'destructive',
			preview_required: true,
			affected_root_ids: ['root-1'],
			reasons: ['automatic trigger enabled']
		});
		h.activation = {
			data: {
				job_id: 'preview-1',
				state: 'running',
				worker_stalled: false,
				control_request: 'none',
				operation_row_revision: 2,
				ready_for_confirmation: false,
				expired: false,
				stale: false,
				summary: { item_count: 20, bundle_count: 2 }
			},
			isLoading: false,
			isError: false,
			isFetching: false,
			refetch: vi.fn()
		};

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();
		await page.getByRole('checkbox', { name: /Acquisitions/ }).click();
		await page.getByRole('button', { name: 'Validate and save' }).click();
		await page.getByRole('button', { name: 'Run dry run' }).click();
		await page.getByRole('button', { name: 'Close', exact: true }).click();

		await page.getByRole('checkbox', { name: /Drop & Free imports/ }).click();
		await expect
			.element(page.getByText('Configuration changed after this dry run started'))
			.toBeVisible();
		await page.getByRole('button', { name: 'Restart review' }).click();

		expect(h.stopActivation).toHaveBeenCalledWith({ jobId: 'preview-1', expectedRevision: 2 });
		await expect.element(page.getByRole('button', { name: 'Run dry run' })).toBeVisible();
		expect(h.createActivation).toHaveBeenCalledOnce();
	});

	it('does not accept an expired or stale activation preview', async () => {
		h.impact.mockResolvedValue({
			current_settings_revision: 'settings-1',
			proposed_settings_revision: 'settings-2',
			stale: false,
			classification: 'destructive',
			preview_required: true,
			affected_root_ids: ['root-1'],
			reasons: []
		});
		h.activation = {
			data: {
				job_id: 'preview-1',
				state: 'ready',
				ready_for_confirmation: true,
				expired: true,
				stale: true,
				summary: { eligible_count: 8, warning_count: 0, blocked_count: 0, path_change_count: 8 }
			},
			isLoading: false,
			refetch: vi.fn()
		};

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();
		await page.getByRole('checkbox', { name: /Acquisitions/ }).click();
		await page.getByRole('button', { name: 'Validate and save' }).click();
		await page.getByRole('button', { name: 'Run dry run' }).click();
		await expect.element(page.getByText(/stale or expired/)).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Use this dry run' })).toBeDisabled();
		await page.getByRole('button', { name: 'Run a fresh dry run' }).click();
		await expect.element(page.getByRole('button', { name: 'Run dry run' })).toBeVisible();
		expect(h.confirmActivation).not.toHaveBeenCalled();
	});

	it('prevents activation dismissal while a destructive confirmation is pending', async () => {
		h.impact.mockResolvedValue({
			current_settings_revision: 'settings-1',
			proposed_settings_revision: 'settings-2',
			stale: false,
			classification: 'destructive',
			preview_required: true,
			affected_root_ids: ['root-1'],
			reasons: []
		});
		h.confirmActivationPending = true;

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();
		await page.getByRole('checkbox', { name: /Acquisitions/ }).click();
		await page.getByRole('button', { name: 'Validate and save' }).click();

		await expect.element(page.getByRole('button', { name: 'Close', exact: true })).toBeDisabled();
		await expect
			.element(page.getByRole('button', { name: 'Close organization activation' }))
			.toBeDisabled();
	});
});
