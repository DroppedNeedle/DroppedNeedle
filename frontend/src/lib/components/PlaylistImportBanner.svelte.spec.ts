import { page } from '@vitest/browser/context';
import { createRawSnippet } from 'svelte';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import PlaylistImportBanner from './PlaylistImportBanner.svelte';

const sourceIcon = createRawSnippet(() => ({
	render: () => '<span data-testid="source-icon"></span>'
}));

async function renderBanner(overrides: Record<string, unknown> = {}) {
	return await render(PlaylistImportBanner, {
		props: {
			sourceLabel: 'Jellyfin',
			playlistsHref: '/library/jellyfin/playlists',
			sourceIcon,
			...overrides
		}
	} as unknown as Parameters<typeof render<typeof PlaylistImportBanner>>[1]);
}

describe('PlaylistImportBanner.svelte', () => {
	it('always explains the unlinked shared-account state', async () => {
		await renderBanner({ accountMode: 'shared', playlists: [] });

		await expect
			.element(page.getByText('No playlists found on the shared Jellyfin account'))
			.toBeVisible();
		await expect.element(page.getByText('Link your account to see your playlists.')).toBeVisible();
		const link = page.getByRole('link', { name: 'Link your account' });
		await expect.element(link).toBeVisible();
		expect(await link.element()).toHaveAttribute('href', '/profile#media-accounts');
	});

	it('shows a connected empty state instead of disappearing', async () => {
		await renderBanner({ accountMode: 'linked', accountLabel: 'alice', playlists: [] });

		await expect.element(page.getByText('No Jellyfin playlists found for alice')).toBeVisible();
		await expect
			.element(page.getByText('alice is connected, but no playlists are available.'))
			.toBeVisible();
		await expect.element(page.getByRole('link', { name: 'Manage account' })).toBeVisible();
	});

	it('keeps its footprint while playlist discovery is loading', async () => {
		await renderBanner({ loading: true });

		await expect.element(page.getByText('Jellyfin')).toBeVisible();
		await expect.element(page.getByRole('group')).toBeVisible();
	});

	it('shows relink guidance for a stale personal credential', async () => {
		await renderBanner({
			accountMode: 'linked',
			errorCode: 'MEDIA_ACCOUNT_RELINK_REQUIRED'
		});

		await expect
			.element(page.getByText('Reconnect Jellyfin to check your playlists'))
			.toBeVisible();
		await expect.element(page.getByRole('link', { name: 'Reconnect Jellyfin' })).toBeVisible();
	});

	it('keeps a visible retry state for generic failures', async () => {
		const onretry = vi.fn();
		await renderBanner({ errorCode: 'EXTERNAL_SERVICE_ERROR', onretry });

		await expect.element(page.getByText("Couldn't check Jellyfin playlists")).toBeVisible();
		await page.getByRole('button', { name: 'Check again' }).click();
		expect(onretry).toHaveBeenCalledOnce();
	});

	it('reports import progress for accessible playlists', async () => {
		await renderBanner({
			accountMode: 'linked',
			accountLabel: 'alice',
			playlists: [
				{
					id: 'one',
					name: 'One',
					track_count: 1,
					duration_seconds: 10,
					cover_url: '',
					is_imported: true
				},
				{
					id: 'two',
					name: 'Two',
					track_count: 2,
					duration_seconds: 20,
					cover_url: '',
					is_imported: false
				}
			]
		});

		await expect
			.element(page.getByText('Bring your 2 Jellyfin playlists to DroppedNeedle'))
			.toBeVisible();
		await expect.element(page.getByText('1 of 2 imported so far.')).toBeVisible();
	});

	it('keeps a completed banner after every playlist is imported', async () => {
		await renderBanner({
			accountMode: 'linked',
			accountLabel: 'alice',
			playlists: [
				{
					id: 'one',
					name: 'One',
					track_count: 1,
					duration_seconds: 10,
					cover_url: '',
					is_imported: true
				}
			]
		});

		await expect.element(page.getByText('All 1 Jellyfin playlist imported')).toBeVisible();
		await expect
			.element(page.getByText('Your Jellyfin playlists are now private copies in DroppedNeedle.'))
			.toBeVisible();
	});
});
