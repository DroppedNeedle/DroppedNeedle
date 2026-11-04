import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import MetadataPanel from '$lib/features/remote-library/components/MetadataPanel.svelte';

type Flagged = { __xss?: string };
const flagged = window as unknown as Flagged;

describe('an address from a connected media server', () => {
	afterEach(() => {
		delete flagged.__xss;
	});

	it('is not rendered as a link when it could run script', async () => {
		await render(MetadataPanel, {
			props: {
				open: true,
				title: 'Blue Lines',
				lastfmUrl: "javascript:window.__xss = 'lastfm'",
				musicbrainzId: '0a1b2c3d-0000-4000-8000-000000000000',
				onclose: () => {}
			}
		});

		await expect.element(page.getByRole('link', { name: 'MusicBrainz' })).toBeVisible();
		await expect.element(page.getByRole('link', { name: 'Last.fm' })).not.toBeInTheDocument();
		expect(document.body.querySelector('a[href^="javascript" i]')).toBeNull();
	});

	it('is rendered as a link when it is a web address', async () => {
		await render(MetadataPanel, {
			props: {
				open: true,
				title: 'Blue Lines',
				lastfmUrl: 'https://www.last.fm/music/Massive+Attack/Blue+Lines',
				onclose: () => {}
			}
		});

		const link = page.getByRole('link', { name: 'Last.fm' });
		await expect
			.element(link)
			.toHaveAttribute('href', 'https://www.last.fm/music/Massive+Attack/Blue+Lines');
		await expect.element(link).toHaveAttribute('target', '_blank');
		await expect.element(link).toHaveAttribute('rel', 'noopener noreferrer');
	});
});
