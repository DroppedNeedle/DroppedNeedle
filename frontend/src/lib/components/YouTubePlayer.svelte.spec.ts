import { page } from '@vitest/browser/context';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import { YOUTUBE_PLAYER_ELEMENT_ID } from '$lib/constants';

import YouTubePlayer from './YouTubePlayer.svelte';

/**
 * YouTube's Developer Policies require an embedded player viewport of at least 200x200
 * and forbid hiding or obscuring it. Breaching either gets the API key revoked. The floor
 * is declared inline on the component so these assertions hold without app.css, which the
 * browser test project does not load.
 */
describe('YouTubePlayer policy compliance', () => {
	it('carries no class that would hide it', async () => {
		await render(YouTubePlayer);

		const container = page.getByTestId('youtube-player');
		await expect.element(container).toBeVisible();

		// toBeVisible() cannot see this: the browser project does not load Tailwind, so a
		// `hidden` utility class never resolves to display:none. Assert on the class list.
		const classes = container.element().className.split(/\s+/);
		expect(classes).not.toContain('hidden');
		expect(classes).not.toContain('invisible');
	});

	it('reserves a viewport of at least 200x200', async () => {
		await render(YouTubePlayer);

		const container = page.getByTestId('youtube-player');
		await expect.element(container).toBeVisible();

		const style = getComputedStyle(container.element());
		expect(Number.parseFloat(style.minWidth)).toBeGreaterThanOrEqual(200);
		expect(Number.parseFloat(style.minHeight)).toBeGreaterThanOrEqual(200);
	});

	it('mounts the element the playback source looks up by id', async () => {
		await render(YouTubePlayer);

		await expect
			.element(page.getByTestId('youtube-player-mount'))
			.toHaveAttribute('id', YOUTUBE_PLAYER_ELEMENT_ID);
	});
});
