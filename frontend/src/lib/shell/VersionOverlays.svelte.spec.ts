import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { dismissWhatsNew } from '$lib/stores/version.svelte';
import { VersionQueryKeyFactory } from '$lib/shell/version.svelte';
import VersionOverlays from './VersionOverlays.svelte';

const noUpdate: RouteData<'GET /api/v3/version/check-update'> = {
	check_failed: false,
	comparison_failed: false,
	current_version: 'v3.1.0',
	update_available: false
};

const releases: RouteData<'GET /api/v3/version/releases'> = [
	{
		tag_name: 'v3.1.0',
		name: 'DroppedNeedle 3.1.0',
		body: 'Albums open faster.',
		prerelease: false,
		published_at: '2026-11-20T00:00:00Z',
		html_url: 'https://github.com/DroppedNeedle/DroppedNeedle/releases/tag/v3.1.0'
	}
];

afterEach(() => localStorage.removeItem('droppedneedle_whats_new_dismissed'));

describe("What's New", () => {
	it('shows the notes for a release not yet closed', async () => {
		dismissWhatsNew('v3.0.0');
		fakeApi({
			'GET /api/v3/version': { version: 'v3.1.0', build_date: null },
			'GET /api/v3/version/check-update': noUpdate,
			'GET /api/v3/version/releases': releases
		});

		await renderWithQuery(VersionOverlays, { updateAvailable: false });

		await expect.element(page.getByText('Albums open faster.')).toBeVisible();
	});

	it('reads no release history once this release was closed', async () => {
		dismissWhatsNew('v3.1.0');
		const api = fakeApi({
			'GET /api/v3/version': { version: 'v3.1.0', build_date: null },
			'GET /api/v3/version/check-update': noUpdate
		});

		const { client } = await renderWithQuery(VersionOverlays, { updateAvailable: false });

		await expect
			.poll(() => client.getQueryData(VersionQueryKeyFactory.info()))
			.toMatchObject({ version: 'v3.1.0' });
		await expect
			.poll(() => client.getQueryState(VersionQueryKeyFactory.releases())?.fetchStatus)
			.toBe('idle');
		expect(api.calls('GET /api/v3/version/releases')).toHaveLength(0);
		expect(page.getByRole('dialog').elements()).toHaveLength(0);
	});
});
