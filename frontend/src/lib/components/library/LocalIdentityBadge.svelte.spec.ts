import { page } from '@vitest/browser/context';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import LocalIdentityBadge from './LocalIdentityBadge.svelte';

describe('LocalIdentityBadge.svelte', () => {
	it.each([
		['local_only', 'Local-only'],
		['release_group_linked', 'Local edition'],
		['custom_edition', 'Custom edition'],
		['release_linked', 'MusicBrainz linked']
	] as const)('shows the album identity state %s', async (state, label) => {
		await render(LocalIdentityBadge, {
			props: { state, subject: 'album', showDescription: true }
		} as Parameters<typeof render<typeof LocalIdentityBadge>>[1]);

		await expect.element(page.getByText(label, { exact: true })).toBeVisible();
	});

	it('describes artist identity independently', async () => {
		await render(LocalIdentityBadge, {
			props: { state: 'musicbrainz_linked', subject: 'artist', showDescription: true }
		} as Parameters<typeof render<typeof LocalIdentityBadge>>[1]);

		await expect.element(page.getByText('This artist is linked to MusicBrainz.')).toBeVisible();
	});

	it('shows a best-fit edition when tags agree on a pressing', async () => {
		await render(LocalIdentityBadge, {
			props: {
				state: 'release_group_linked',
				subject: 'album',
				showDescription: true,
				pickBasis: 'embedded_tags'
			}
		} as Parameters<typeof render<typeof LocalIdentityBadge>>[1]);

		await expect.element(page.getByText('Best-fit edition', { exact: true })).toBeVisible();
		await expect
			.element(
				page.getByText('Your files agree on this pressing, shown as the best fit until verified.')
			)
			.toBeVisible();
	});

	it('shows a best-fit edition when the pressing is pinned', async () => {
		await render(LocalIdentityBadge, {
			props: {
				state: 'release_group_linked',
				subject: 'album',
				showDescription: true,
				pickBasis: 'pin'
			}
		} as Parameters<typeof render<typeof LocalIdentityBadge>>[1]);

		await expect.element(page.getByText('Best-fit edition', { exact: true })).toBeVisible();
	});

	it('shows a best-fit edition when the pressing matches the identity', async () => {
		await render(LocalIdentityBadge, {
			props: {
				state: 'release_group_linked',
				subject: 'album',
				showDescription: true,
				pickBasis: 'owned'
			}
		} as Parameters<typeof render<typeof LocalIdentityBadge>>[1]);

		await expect.element(page.getByText('Best-fit edition', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('This pressing matches your identified edition and is shown.'))
			.toBeVisible();
	});
});
