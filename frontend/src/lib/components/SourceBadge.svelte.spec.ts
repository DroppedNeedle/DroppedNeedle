import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import SourceBadge from './SourceBadge.svelte';

describe('SourceBadge', () => {
	it('shows one service', async () => {
		render(SourceBadge, { source: 'lastfm' });
		await expect.element(page.getByText('Last.fm')).toBeInTheDocument();
		expect(page.getByText('ListenBrainz').elements()).toHaveLength(0);
	});

	it('shows both services for a blended shelf', async () => {
		render(SourceBadge, { source: 'lastfm_and_listenbrainz' });
		await expect.element(page.getByText('Last.fm')).toBeInTheDocument();
		await expect.element(page.getByText('ListenBrainz')).toBeInTheDocument();
		expect(page.getByText('lastfm_and_listenbrainz').elements()).toHaveLength(0);
	});
});
