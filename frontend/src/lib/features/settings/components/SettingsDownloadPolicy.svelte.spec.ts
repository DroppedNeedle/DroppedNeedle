import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { DownloadPolicySettings } from '$lib/features/downloads/queries/DownloadClientsQueries.svelte';

import type { QualityRecipeEntry } from './acquisition/qualityRecipeModel';

const recipe: QualityRecipeEntry[] = [
	{ format: 'flac', quality: 'cd' },
	{
		format: 'mp3',
		quality: '320_plus',
		min_bitrate_kbps: 320,
		max_bitrate_kbps: null
	}
];

const basePolicy: DownloadPolicySettings = {
	quality_min: 'mp3_320',
	quality_max: 'lossless',
	preflight_score_auto_accept: 0.7,
	preflight_score_manual_min: 0.5,
	download_stall_timeout_minutes: 30,
	download_queued_timeout_minutes: 120,
	preferred_quality_wait_minutes: 15,
	max_failover_attempts: 3,
	max_concurrent_downloads: 3,
	auto_retry_enabled: true,
	auto_retry_max_attempts: 6,
	auto_retry_base_interval_minutes: 15,
	quality_cutoff: 'lossless',
	upgrade_allowed: false,
	max_library_size_gb: 0,
	default_request_quota_count: 0,
	default_request_quota_days: 7,
	default_storage_quota_gb: 0,
	background_upgrade_scan_enabled: false,
	background_upgrade_scan_interval_hours: 12,
	background_upgrade_max_per_run: 3,
	ignored_terms: [],
	required_terms: [],
	max_size_mb: 0,
	search_review: 'when_unsure',
	usenet_retention_days: 0,
	quality_recipe: recipe,
	quality_recipe_status: 'valid',
	quality_recipe_error: null
};

const h = vi.hoisted(() => ({
	policy: undefined as unknown,
	pending: false,
	isError: false,
	error: null as unknown,
	refetch: vi.fn(),
	mutateAsync: vi.fn()
}));

vi.mock('$lib/features/downloads/queries/DownloadClientsQueries.svelte', () => ({
	getDownloadPolicyQuery: () => ({
		get data() {
			return h.policy;
		},
		get isPending() {
			return h.pending;
		},
		get isError() {
			return h.isError;
		},
		get error() {
			return h.error;
		},
		refetch: h.refetch
	}),
	createSaveDownloadPolicyMutation: () => ({
		mutateAsync: h.mutateAsync,
		get isPending() {
			return false;
		}
	})
}));

import SettingsDownloadPolicy from './SettingsDownloadPolicy.svelte';

function cutoffSelect(): HTMLSelectElement {
	return page
		.getByRole('combobox', { name: 'Upgrade until quality reaches' })
		.element() as HTMLSelectElement;
}

function rows(container: HTMLElement): HTMLElement[] {
	return [...container.querySelectorAll('[data-motion="quality-recipe"] ol > li')] as HTMLElement[];
}

describe('SettingsDownloadPolicy quality recipe', () => {
	beforeEach(() => {
		h.policy = structuredClone(basePolicy);
		h.pending = false;
		h.isError = false;
		h.error = null;
		h.refetch = vi.fn();
		h.mutateAsync = vi.fn(async (policy: DownloadPolicySettings) => structuredClone(policy));
	});

	it('sends the edited order and custom recipe values in the mutation payload', async () => {
		const { container } = await render(SettingsDownloadPolicy);
		await page.getByRole('radio', { name: 'Custom' }).nth(1).click();
		await page.getByRole('spinbutton', { name: 'Custom MP3 minimum bitrate' }).fill('16');
		await page.getByRole('spinbutton', { name: 'Custom MP3 maximum bitrate' }).fill('191');
		await page.getByRole('button', { name: 'Add MP3 recipe entry' }).click();
		await page.getByRole('button', { name: 'Move MP3 · Custom · 16-191 kbps up' }).click();
		await page.getByRole('button', { name: 'Save acquisition policy' }).click();

		const saved = h.mutateAsync.mock.calls[0][0] as Record<string, unknown>;
		expect(saved.quality_recipe).toEqual([
			recipe[0],
			{
				format: 'mp3',
				quality: 'custom',
				min_bitrate_kbps: 16,
				max_bitrate_kbps: 191
			},
			recipe[1]
		]);
		expect(container.querySelectorAll('[data-recipe-id]')).toHaveLength(3);
	});

	it('saves the range and cutoff the recipe covers, from custom MP3 bounds or a removed FLAC entry', async () => {
		{
			// Custom MP3 coverage sets the live range and clamps the cutoff.
			h.policy = {
				...structuredClone(basePolicy),
				quality_recipe: [
					{
						format: 'mp3',
						quality: 'custom',
						min_bitrate_kbps: 180,
						max_bitrate_kbps: 300
					}
				]
			};
			const view = await render(SettingsDownloadPolicy);
			const cutoff = cutoffSelect();
			await expect.element(cutoff).toHaveValue('mp3_256');
			expect(
				[...cutoff.options].filter((option) => !option.disabled).map((option) => option.value)
			).toEqual(['low', 'mp3_192', 'mp3_256']);

			await page.getByRole('button', { name: 'Save acquisition policy' }).click();
			const saved = h.mutateAsync.mock.calls[0][0] as Record<string, unknown>;
			expect(saved.quality_min).toBe('low');
			expect(saved.quality_max).toBe('mp3_256');
			expect(saved.quality_cutoff).toBe('mp3_256');
			view.unmount();
		}

		{
			// Removing the FLAC entry narrows the range and clamps the cutoff.
			h.mutateAsync = vi.fn(async (policy: DownloadPolicySettings) => structuredClone(policy));
			h.policy = {
				...structuredClone(basePolicy),
				quality_min: 'low',
				quality_max: 'lossless',
				quality_recipe: [
					{ format: 'flac', quality: 'cd' },
					{
						format: 'mp3',
						quality: '320_plus',
						min_bitrate_kbps: 320,
						max_bitrate_kbps: null
					},
					{
						format: 'mp3',
						quality: '256_319',
						min_bitrate_kbps: 256,
						max_bitrate_kbps: 319
					},
					{
						format: 'mp3',
						quality: '192_255',
						min_bitrate_kbps: 192,
						max_bitrate_kbps: 255
					},
					{
						format: 'mp3',
						quality: 'below_192',
						min_bitrate_kbps: 16,
						max_bitrate_kbps: 191
					}
				]
			};
			const { container } = await render(SettingsDownloadPolicy);
			expect(rows(container)).toHaveLength(5);
			await page.getByRole('button', { name: 'Remove FLAC · CD quality' }).click();
			expect(rows(container)).toHaveLength(4);
			await page.getByRole('button', { name: 'Save acquisition policy' }).click();

			expect(h.mutateAsync).toHaveBeenCalledTimes(1);
			const saved = h.mutateAsync.mock.calls[0][0] as Record<string, unknown>;
			expect(saved.quality_min).toBe('low');
			expect(saved.quality_max).toBe('mp3_320');
			expect(saved.quality_cutoff).toBe('mp3_320');
		}
	});
});
