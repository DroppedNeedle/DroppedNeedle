import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { DownloadPolicySettings } from '$lib/types';

const basePolicy: DownloadPolicySettings = {
	quality_min: 'mp3_320',
	quality_max: 'lossless',
	flac_mp3_only: true,
	verify_downloads: true,
	preflight_score_auto_accept: 0.7,
	preflight_score_manual_min: 0.5,
	download_stall_timeout_minutes: 30,
	download_queued_timeout_minutes: 120,
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
	background_upgrade_max_per_run: 3
};

const h = vi.hoisted(() => ({
	policy: undefined as unknown,
	mutateAsync: vi.fn()
}));

vi.mock('$lib/queries/downloads/DownloadClientsQueries.svelte', () => ({
	getDownloadPolicyQuery: () => ({
		get data() {
			return h.policy;
		}
	}),
	saveDownloadPolicy: () => ({
		mutateAsync: h.mutateAsync,
		isPending: false
	})
}));

import SettingsDownloadPolicy from './SettingsDownloadPolicy.svelte';

function cutoffSelect(container: HTMLElement): HTMLSelectElement {
	const select = container.querySelector('select');
	if (!select) throw new Error('cutoff select not rendered');
	return select;
}

describe('SettingsDownloadPolicy upgrade controls', () => {
	beforeEach(() => {
		h.policy = { ...basePolicy };
		h.mutateAsync = vi.fn().mockResolvedValue(undefined);
	});

	it('seeds the cutoff and upgrades toggle from the saved policy', async () => {
		h.policy = { ...basePolicy, quality_cutoff: 'mp3_320', upgrade_allowed: true };
		const { container } = render(SettingsDownloadPolicy);

		await expect
			.element(page.getByRole('checkbox', { name: 'Allow automatic upgrades' }))
			.toBeChecked();
		expect(cutoffSelect(container).value).toBe('mp3_320');
	});

	it('disables cutoff options outside the accepted quality band', async () => {
		h.policy = { ...basePolicy, quality_min: 'mp3_256', quality_max: 'mp3_320' };
		const { container } = render(SettingsDownloadPolicy);
		await expect
			.element(page.getByRole('checkbox', { name: 'Allow automatic upgrades' }))
			.toBeVisible();

		const disabledByKey = Object.fromEntries(
			Array.from(cutoffSelect(container).options).map((o) => [o.value, o.disabled])
		);
		expect(disabledByKey).toEqual({
			low: true,
			mp3_192: true,
			mp3_256: false,
			mp3_320: false,
			lossless: true
		});
	});

	it('clamps a cutoff that falls outside the band to the nearest edge', async () => {
		h.policy = {
			...basePolicy,
			quality_min: 'mp3_192',
			quality_max: 'mp3_256',
			quality_cutoff: 'lossless'
		};
		const { container } = render(SettingsDownloadPolicy);
		await expect
			.element(page.getByRole('checkbox', { name: 'Allow automatic upgrades' }))
			.toBeVisible();

		expect(cutoffSelect(container).value).toBe('mp3_256');
	});

	it('saves the cutoff and toggle through the policy mutation', async () => {
		render(SettingsDownloadPolicy);

		await page.getByRole('checkbox', { name: 'Allow automatic upgrades' }).click();
		await page.getByRole('button', { name: 'Save' }).click();

		expect(h.mutateAsync).toHaveBeenCalledTimes(1);
		const saved = h.mutateAsync.mock.calls[0][0] as DownloadPolicySettings;
		expect(saved.upgrade_allowed).toBe(true);
		expect(saved.quality_cutoff).toBe('lossless');
	});
});
