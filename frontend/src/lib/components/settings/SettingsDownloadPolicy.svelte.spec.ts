import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { DownloadPolicySettings, QualityRecipeEntry } from '$lib/types';
import { PRESETS } from './acquisition/qualityRecipeModel';

const recipe: QualityRecipeEntry[] = [
	{ format: 'flac', quality: 'cd' },
	{
		format: 'mp3',
		quality: '320_plus',
		min_bitrate_kbps: 320,
		target_bitrate_kbps: 320,
		max_bitrate_kbps: null
	}
];

const basePolicy: DownloadPolicySettings = {
	quality_min: 'mp3_320',
	quality_max: 'lossless',
	flac_mp3_only: true,
	verify_downloads: true,
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
	usenet_min_release_age_minutes: 30,
	quality_cutoff: 'lossless',
	upgrade_allowed: false,
	max_library_size_gb: 0,
	default_request_quota_count: 0,
	default_request_quota_days: 7,
	default_storage_quota_gb: 0,
	background_upgrade_scan_enabled: false,
	background_upgrade_scan_interval_hours: 12,
	background_upgrade_max_per_run: 3,
	quality_preference_order: ['lossless', 'mp3_320'],
	preferred_lossy_bitrate_kbps: 320,
	lossy_min_bitrate_kbps: null,
	lossy_max_bitrate_kbps: null,
	lossless_preference: 'cd',
	lossless_max_bit_depth: 16,
	lossless_max_sample_rate_hz: 48000,
	unknown_quality_behavior: 'review',
	source_selection_mode: 'source_first',
	quality_recipe: recipe,
	quality_recipe_status: 'v2',
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

vi.mock('$lib/queries/downloads/DownloadClientsQueries.svelte', () => ({
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
	saveDownloadPolicy: () => ({
		mutateAsync: h.mutateAsync,
		get isPending() {
			return false;
		}
	})
}));

import SettingsDownloadPolicy from './SettingsDownloadPolicy.svelte';

function cutoffSelect(container: HTMLElement): HTMLSelectElement {
	const el = [...container.querySelectorAll('select')].find((select) =>
		select.getAttribute('aria-label')?.includes('Upgrade')
	);
	if (!el) throw new Error('cutoff select not rendered');
	return el as HTMLSelectElement;
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
		h.mutateAsync = vi.fn().mockResolvedValue(undefined);
	});

	it('loads the saved recipe, summary, current preset, and exactly one acquisition policy save action', async () => {
		h.policy = {
			...structuredClone(basePolicy),
			quality_recipe: structuredClone(PRESETS.balanced.recipe)
		};
		const { container } = render(SettingsDownloadPolicy);

		await expect
			.element(
				page.getByText(
					'Try FLAC · CD quality → MP3 · 320+ kbps → MP3 · 256-319 kbps → MP3 · 192-255 kbps.',
					{ exact: true }
				)
			)
			.toBeVisible();
		expect(container.querySelector('[data-preset="balanced"] .badge')?.textContent?.trim()).toBe(
			'Current'
		);
		expect(rows(container)).toHaveLength(4);
		expect(container.querySelectorAll('button[aria-label="Save acquisition policy"]')).toHaveLength(
			1
		);
	});

	it('keeps upgrade controls in the same save mutation as the recipe', async () => {
		const { container } = render(SettingsDownloadPolicy);
		await page.getByRole('checkbox', { name: 'Allow automatic upgrades' }).click();
		await page.getByRole('button', { name: 'Save acquisition policy' }).click();

		expect(h.mutateAsync).toHaveBeenCalledTimes(1);
		const saved = h.mutateAsync.mock.calls[0][0] as Record<string, unknown>;
		expect(saved.upgrade_allowed).toBe(true);
		expect(saved.quality_cutoff).toBe('lossless');
		expect(saved.quality_recipe_status).toBe('v2');
		expect(saved.quality_recipe).toEqual(recipe);
		expect(cutoffSelect(container).value).toBe('lossless');
	});
	it('derives the live legacy range from custom MP3 coverage and clamps the cutoff', async () => {
		h.policy = {
			...structuredClone(basePolicy),
			quality_recipe: [
				{
					format: 'mp3',
					quality: 'custom',
					min_bitrate_kbps: 180,
					target_bitrate_kbps: 224,
					max_bitrate_kbps: 300
				}
			]
		};
		const { container } = render(SettingsDownloadPolicy);
		const cutoff = cutoffSelect(container);
		await expect.element(cutoff).toHaveValue('mp3_256');
		expect(
			[...cutoff.options].filter((option) => !option.disabled).map((option) => option.value)
		).toEqual(['low', 'mp3_192', 'mp3_256']);

		await page.getByRole('button', { name: 'Save acquisition policy' }).click();
		const saved = h.mutateAsync.mock.calls[0][0] as Record<string, unknown>;
		expect(saved.quality_min).toBe('low');
		expect(saved.quality_max).toBe('mp3_256');
		expect(saved.quality_cutoff).toBe('mp3_256');
	});

	it('sends the edited order and custom recipe values in the mutation payload', async () => {
		const { container } = render(SettingsDownloadPolicy);
		await page.getByRole('radio', { name: 'Custom' }).nth(1).click();
		await page.getByRole('spinbutton', { name: 'Custom MP3 minimum bitrate' }).fill('16');
		await page.getByRole('spinbutton', { name: 'Custom MP3 target bitrate' }).fill('160');
		await page.getByRole('spinbutton', { name: 'Custom MP3 maximum bitrate' }).fill('191');
		await page.getByRole('button', { name: 'Add MP3 recipe entry' }).click();
		await page.getByRole('button', { name: 'Move MP3 · Custom · 16-160-191 kbps up' }).click();
		await page.getByRole('button', { name: 'Save acquisition policy' }).click();

		const saved = h.mutateAsync.mock.calls[0][0] as Record<string, unknown>;
		expect(saved.quality_recipe).toEqual([
			recipe[0],
			{
				format: 'mp3',
				quality: 'custom',
				min_bitrate_kbps: 16,
				target_bitrate_kbps: 160,
				max_bitrate_kbps: 191
			},
			recipe[1]
		]);
		expect(container.querySelectorAll('[data-recipe-id]')).toHaveLength(3);
	});

	it('adds, edits, removes, and reorders entries through the composed settings surface', async () => {
		const { container } = render(SettingsDownloadPolicy);
		await page.getByRole('radio', { name: /24-bit \/ 96 kHz/ }).click();
		await page.getByRole('button', { name: 'Add FLAC recipe entry' }).click();
		expect(rows(container)).toHaveLength(3);
		expect(rows(container)[2]).toHaveTextContent('24-bit / 96 kHz');

		await page.getByRole('button', { name: 'Move FLAC · 24-bit / 96 kHz up' }).click();
		expect(rows(container)[1]).toHaveTextContent('24-bit / 96 kHz');

		await page.getByRole('button', { name: 'Edit FLAC · 24-bit / 96 kHz' }).click();
		await page.getByRole('radio', { name: /^24-bit \/ 192 kHz High-resolution/ }).click();
		await page.getByRole('button', { name: 'Update FLAC recipe entry' }).click();
		expect(rows(container)[1]).toHaveTextContent('24-bit / 192 kHz');

		await page.getByRole('button', { name: 'Remove FLAC · 24-bit / 192 kHz' }).click();
		expect(rows(container)).toHaveLength(2);
	});

	it('blocks inclusive MP3 overlap and accepts a non-overlapping custom region', async () => {
		h.policy = {
			...structuredClone(basePolicy),
			quality_recipe: [
				{ format: 'flac', quality: 'cd' },
				{
					format: 'mp3',
					quality: '192_255',
					min_bitrate_kbps: 192,
					target_bitrate_kbps: 192,
					max_bitrate_kbps: 255
				}
			]
		};
		const { container } = render(SettingsDownloadPolicy);
		await page.getByRole('radio', { name: 'Custom' }).nth(1).click();
		await page.getByRole('spinbutton', { name: 'Custom MP3 minimum bitrate' }).fill('192');
		await page.getByRole('spinbutton', { name: 'Custom MP3 target bitrate' }).fill('224');
		await page.getByRole('spinbutton', { name: 'Custom MP3 maximum bitrate' }).fill('255');
		await expect.element(page.getByRole('button', { name: 'Add MP3 recipe entry' })).toBeDisabled();
		await expect.element(page.getByRole('alert').nth(1)).toHaveTextContent(/overlaps/i);

		await page.getByRole('spinbutton', { name: 'Custom MP3 minimum bitrate' }).fill('16');
		await page.getByRole('spinbutton', { name: 'Custom MP3 target bitrate' }).fill('160');
		await page.getByRole('spinbutton', { name: 'Custom MP3 maximum bitrate' }).fill('191');
		await expect.element(page.getByRole('button', { name: 'Add MP3 recipe entry' })).toBeEnabled();
		await page.getByRole('button', { name: 'Add MP3 recipe entry' }).click();
		expect(rows(container)).toHaveLength(3);
		expect(rows(container)[2]).toHaveTextContent('Custom · 16-160-191 kbps');
	});
	it('rejects duplicate custom FLAC resolution with a position-aware error', async () => {
		const { container } = render(SettingsDownloadPolicy);
		await page.getByRole('radio', { name: 'Custom' }).first().click();
		await page.getByRole('spinbutton', { name: 'Custom FLAC bit depth' }).fill('24');
		await page.getByRole('spinbutton', { name: 'Custom FLAC sample rate' }).fill('96000');
		await page.getByRole('button', { name: 'Add FLAC recipe entry' }).click();

		await page.getByRole('radio', { name: 'Custom' }).first().click();
		await page.getByRole('spinbutton', { name: 'Custom FLAC bit depth' }).fill('24');
		await page.getByRole('spinbutton', { name: 'Custom FLAC sample rate' }).fill('96000');
		await expect
			.element(page.getByRole('button', { name: 'Add FLAC recipe entry' }))
			.toBeDisabled();
		await expect
			.element(
				page.getByText('This exact FLAC resolution is already at position 3.', { exact: true })
			)
			.toBeVisible();
		expect(rows(container)).toHaveLength(3);
	});

	it('requires confirmation before replacing dirty edits with a preset and lets Cancel preserve them', async () => {
		const { container } = render(SettingsDownloadPolicy);
		await page.getByRole('radio', { name: /24-bit \/ 96 kHz/ }).click();
		await page.getByRole('button', { name: 'Add FLAC recipe entry' }).click();
		const trigger = page.getByRole('button', { name: 'Apply Best available preset' });
		await trigger.click();
		const dialog = container.querySelector('dialog.modal') as HTMLDialogElement | null;
		expect(dialog?.open).toBe(true);
		expect(dialog?.getAttribute('aria-labelledby')).toBe('quality-recipe-dialog-title');
		expect(dialog?.getAttribute('aria-describedby')).toBe('quality-recipe-dialog-description');
		expect(
			[
				...container.querySelectorAll(
					'[role="radiogroup"][aria-label="Source selection mode"] label'
				)
			].every((label) => label.classList.contains('min-h-11'))
		).toBe(true);
		await page.getByRole('button', { name: 'Cancel' }).click();
		expect(rows(container)[0]).toHaveTextContent('CD quality');
		expect(container.ownerDocument.activeElement?.getAttribute('data-preset')).toBe(
			'best_available'
		);
	});

	it('projects legacy v1 lossless preference into all FLAC buckets and displays the migration notice', async () => {
		h.policy = {
			...structuredClone(basePolicy),
			quality_recipe: [],
			quality_recipe_status: 'v1',
			quality_recipe_error: null,
			quality_preference_order: ['lossless', 'mp3_320'],
			lossless_preference: '24_96'
		};
		const { container } = render(SettingsDownloadPolicy);
		await expect.element(page.getByRole('alert').first()).toHaveTextContent(/projected/i);
		expect(
			rows(container)
				.slice(0, 5)
				.map((row) => row.textContent)
		).toEqual([
			expect.stringContaining('24-bit / 96 kHz'),
			expect.stringContaining('24-bit / 192 kHz'),
			expect.stringContaining('Above 24-bit / 192 kHz'),
			expect.stringContaining('CD quality'),
			expect.stringContaining('24-bit / 48 kHz')
		]);
		await page.getByRole('button', { name: 'Save acquisition policy' }).click();
		const saved = h.mutateAsync.mock.calls[0][0] as Record<string, unknown>;
		expect(saved.quality_recipe_status).toBe('v2');
		expect(saved.quality_recipe).toEqual([
			{ format: 'flac', quality: '24_96' },
			{ format: 'flac', quality: '24_192' },
			{ format: 'flac', quality: 'hi_res' },
			{ format: 'flac', quality: 'cd' },
			{ format: 'flac', quality: '24_48' },
			{
				format: 'mp3',
				quality: '320_plus',
				min_bitrate_kbps: 320,
				target_bitrate_kbps: 320,
				max_bitrate_kbps: null
			}
		]);
	});
	it('shows replacement state instead of presenting a non-convertible saved recipe', async () => {
		h.policy = {
			...structuredClone(basePolicy),
			flac_mp3_only: false,
			quality_recipe_status: 'non_convertible',
			quality_recipe_error: 'The saved policy allows additional formats.',
			quality_recipe: structuredClone(recipe)
		};
		const { container } = render(SettingsDownloadPolicy);

		await expect
			.element(page.getByRole('alert').first())
			.toHaveTextContent('The saved policy allows additional formats.');
		expect(rows(container)).toHaveLength(0);
		await expect.element(page.getByRole('button', { name: 'Apply Balanced preset' })).toBeVisible();
	});

	it('shows loading without rendering editable defaults', async () => {
		h.policy = undefined;
		h.pending = true;
		const { container } = render(SettingsDownloadPolicy);
		await expect
			.element(page.getByRole('region', { name: 'Loading acquisition policy' }))
			.toHaveAttribute('aria-busy', 'true');
		expect(container.querySelector('[data-motion="quality-recipe"]')).toBeNull();
	});

	it('shows an error and retries without rendering editable defaults', async () => {
		h.policy = undefined;
		h.pending = false;
		h.isError = true;
		h.error = new Error('network');
		const { container } = render(SettingsDownloadPolicy);
		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent('Could not load acquisition policy');
		expect(container.querySelector('[data-motion="quality-recipe"]')).toBeNull();
		await page.getByRole('button', { name: 'Retry' }).click();
		expect(h.refetch).toHaveBeenCalledTimes(1);
	});

	it('preserves the draft after a failed save and restores it with Discard', async () => {
		h.mutateAsync = vi.fn().mockRejectedValue(new Error('conflict'));
		const { container } = render(SettingsDownloadPolicy);
		await page.getByRole('radio', { name: /24-bit \/ 96 kHz/ }).click();
		await page.getByRole('button', { name: 'Add FLAC recipe entry' }).click();
		await page.getByRole('button', { name: 'Save acquisition policy' }).click();
		await expect.element(page.getByRole('button', { name: 'Discard' })).toBeVisible();
		expect(rows(container)).toHaveLength(3);
		await page.getByRole('button', { name: 'Discard' }).click();
		expect(rows(container)).toHaveLength(2);
	});

	it('heals a stale preference order when removing the FLAC entry narrows the range', async () => {
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
					target_bitrate_kbps: 320,
					max_bitrate_kbps: null
				},
				{
					format: 'mp3',
					quality: '256_319',
					min_bitrate_kbps: 256,
					target_bitrate_kbps: 256,
					max_bitrate_kbps: 319
				},
				{
					format: 'mp3',
					quality: '192_255',
					min_bitrate_kbps: 192,
					target_bitrate_kbps: 192,
					max_bitrate_kbps: 255
				},
				{
					format: 'mp3',
					quality: 'below_192',
					min_bitrate_kbps: 16,
					target_bitrate_kbps: 128,
					max_bitrate_kbps: 191
				}
			],
			quality_preference_order: ['lossless', 'mp3_320', 'mp3_256', 'mp3_192', 'low']
		};
		const { container } = render(SettingsDownloadPolicy);
		expect(rows(container)).toHaveLength(5);
		await page.getByRole('button', { name: 'Remove FLAC · CD quality' }).click();
		expect(rows(container)).toHaveLength(4);
		await page.getByRole('button', { name: 'Save acquisition policy' }).click();

		expect(h.mutateAsync).toHaveBeenCalledTimes(1);
		const saved = h.mutateAsync.mock.calls[0][0] as Record<string, unknown>;
		expect(saved.quality_min).toBe('low');
		expect(saved.quality_max).toBe('mp3_320');
		expect(saved.quality_cutoff).toBe('mp3_320');
		const order = saved.quality_preference_order as string[];
		const expected = ['mp3_320', 'mp3_256', 'mp3_192', 'low'];
		expect(order.length === 0 || [...order].sort().join() === [...expected].sort().join()).toBe(
			true
		);
		expect(order).not.toEqual(['lossless', 'mp3_320', 'mp3_256', 'mp3_192', 'low']);
	});

	it('heals a stale preference order when adding an entry widens the range', async () => {
		const { container } = render(SettingsDownloadPolicy);
		expect(rows(container)).toHaveLength(2);
		await page.getByRole('radio', { name: 'Below 192' }).click();
		await page.getByRole('button', { name: 'Add MP3 recipe entry' }).click();
		expect(rows(container)).toHaveLength(3);
		await page.getByRole('button', { name: 'Save acquisition policy' }).click();

		expect(h.mutateAsync).toHaveBeenCalledTimes(1);
		const saved = h.mutateAsync.mock.calls[0][0] as Record<string, unknown>;
		expect(saved.quality_min).toBe('low');
		expect(saved.quality_max).toBe('lossless');
		const order = saved.quality_preference_order as string[];
		const expected = ['lossless', 'mp3_320', 'mp3_256', 'mp3_192', 'low'];
		expect(order.length === 0 || [...order].sort().join() === [...expected].sort().join()).toBe(
			true
		);
		expect(order).not.toEqual(['lossless', 'mp3_320']);
	});

	it('preserves the stored preference-order permutation when the range is unchanged', async () => {
		h.policy = {
			...structuredClone(basePolicy),
			quality_preference_order: ['mp3_320', 'lossless']
		};
		render(SettingsDownloadPolicy);
		await page.getByRole('button', { name: 'Save acquisition policy' }).click();

		expect(h.mutateAsync).toHaveBeenCalledTimes(1);
		const saved = h.mutateAsync.mock.calls[0][0] as Record<string, unknown>;
		expect(saved.quality_min).toBe('mp3_320');
		expect(saved.quality_max).toBe('lossless');
		expect(saved.quality_preference_order).toEqual(['mp3_320', 'lossless']);
	});
});
