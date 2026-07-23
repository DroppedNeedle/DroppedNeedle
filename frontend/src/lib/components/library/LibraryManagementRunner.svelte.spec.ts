import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { LibraryManagementSettingsResponse } from '$lib/queries/library-management/types';

const h = vi.hoisted(() => ({
	createPreview: vi.fn(),
	createRestore: vi.fn(),
	goto: vi.fn()
}));

vi.mock('$app/navigation', () => ({ goto: h.goto }));
vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibrarySearchQuery: () => ({
		data: {
			artists: [],
			albums: [],
			tracks: [
				{
					id: 'track-1',
					title: 'The Track',
					artist_name: 'The Artist',
					album_title: 'The Album'
				}
			]
		},
		isLoading: false,
		isError: false
	})
}));
vi.mock('$lib/queries/library-management/LibraryManagementMutations.svelte', () => ({
	createLibraryManagementPreviewMutation: () => ({
		mutateAsync: h.createPreview,
		isPending: false
	}),
	createLibraryManagementBaselineRestorePreviewMutation: () => ({
		mutateAsync: h.createRestore,
		isPending: false
	})
}));

import LibraryManagementRunner from './LibraryManagementRunner.svelte';

const roots = [
	{ id: 'root-1', label: 'Archive', path: '/music', policy: 'automatic' as const, rules: [] }
];

const settings = {
	default_profile_id: 'profile-1',
	settings_revision: 'settings-1',
	profiles: [
		{
			id: 'profile-1',
			name: 'Picard-style Organizer',
			description: 'Writes canonical tags and organizes album bundles.',
			metadata: { enabled: true },
			genres: { enabled: true },
			artwork: { embedded_enabled: true, external_enabled: true },
			organization: { rename_enabled: true, move_enabled: true, move_sidecars: true }
		}
	]
} as unknown as LibraryManagementSettingsResponse;

beforeEach(() => {
	vi.clearAllMocks();
	sessionStorage.clear();
	h.createPreview.mockResolvedValue({
		job_id: 'preview-1',
		preview_token: 'secret-token',
		created_at: 1,
		expires_at: 2,
		existing: false
	});
	h.createRestore.mockResolvedValue({
		job_id: 'restore-preview-1',
		preview_token: 'restore-token',
		created_at: 1,
		expires_at: 2,
		existing: false
	});
});

describe('LibraryManagementRunner', () => {
	it('discloses track-to-album expansion and creates only a durable preview', async () => {
		render(LibraryManagementRunner, {
			roots,
			settings,
			policyRevision: 'policy-1',
			onclose: vi.fn()
		});

		await expect
			.element(page.getByRole('heading', { name: 'Preview Library Management' }))
			.toHaveFocus();
		await page.getByRole('tab', { name: 'Tracks' }).click();
		await page.getByRole('textbox', { name: 'Search library tracks' }).fill('track');
		await page.getByRole('checkbox', { name: /The Track/ }).click();
		await expect.element(page.getByRole('button', { name: /Continue/ })).toBeEnabled();
		expect(h.createPreview).not.toHaveBeenCalled();

		await page.getByRole('button', { name: /Continue/ }).click();
		await page.getByRole('button', { name: /Continue/ }).click();
		await page.getByRole('checkbox', { name: /Customize this run/ }).click();
		await page.getByRole('checkbox', { name: /Embedded artwork/ }).click();
		await page.getByRole('button', { name: /Continue/ }).click();

		await expect.element(page.getByText(/expands to complete albums/)).toBeVisible();
		expect(h.createPreview).not.toHaveBeenCalled();
		await page.getByRole('button', { name: 'Generate preview' }).click();

		expect(h.createPreview).toHaveBeenCalledWith(
			expect.objectContaining({
				selection: { kind: 'tracks', ids: ['track-1'] },
				profile_id: 'profile-1',
				expected_settings_revision: 'settings-1',
				expected_policy_revision: 'policy-1',
				overrides: expect.objectContaining({ embedded_artwork_enabled: false })
			})
		);
		expect(sessionStorage.getItem('droppedneedle:library-management:preview-token:preview-1')).toBe(
			'secret-token'
		);
		expect(h.goto).toHaveBeenCalledWith('/library/management/previews/preview-1');
	});

	it('labels baseline restore as broader than Undo', async () => {
		render(LibraryManagementRunner, {
			mode: 'baseline_restore',
			roots,
			settings,
			policyRevision: 'policy-1',
			onclose: vi.fn()
		});
		await expect
			.element(page.getByRole('heading', { name: 'Restore first-management baselines' }))
			.toBeVisible();
		await page.getByRole('button', { name: /Continue/ }).click();
		await expect.element(page.getByText(/separate from Undo/)).toBeVisible();
	});

	it('can generate a baseline restore independently of the current profile', async () => {
		const settingsWithoutProfile = {
			...settings,
			default_profile_id: 'missing-profile',
			profiles: []
		} as unknown as LibraryManagementSettingsResponse;
		render(LibraryManagementRunner, {
			mode: 'baseline_restore',
			roots,
			settings: settingsWithoutProfile,
			policyRevision: 'policy-1',
			onclose: vi.fn()
		});

		await page.getByRole('button', { name: /Continue/ }).click();
		await page.getByRole('button', { name: 'Generate preview' }).click();
		expect(h.createRestore).toHaveBeenCalledWith(
			expect.objectContaining({
				selection: { kind: 'roots', ids: ['root-1'] },
				expected_settings_revision: 'settings-1',
				expected_policy_revision: 'policy-1'
			})
		);
		expect(h.goto).toHaveBeenCalledWith('/library/management/previews/restore-preview-1');
	});
});
