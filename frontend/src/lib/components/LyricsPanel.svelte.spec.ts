import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import LyricsPanel from './LyricsPanel.svelte';

describe('LyricsPanel', () => {
	it('highlights the synchronized line at the current playback time', async () => {
		await render(LyricsPanel, {
			open: true,
			lyricsText: 'First line\nSecond line\nThird line',
			lines: [
				{ text: 'First line', start_seconds: 0 },
				{ text: 'Second line', start_seconds: 5 },
				{ text: 'Third line', start_seconds: 10 }
			],
			isSynced: true,
			currentTime: 7,
			onclose: vi.fn()
		});

		await expect
			.element(page.getByText('Second line', { exact: true }))
			.toHaveClass(/text-primary/);
		await expect.element(page.getByText('First line', { exact: true })).toHaveClass(/opacity-80/);
		await expect.element(page.getByText('Third line', { exact: true })).toHaveClass(/opacity-40/);
		await expect.element(page.getByText('Synced', { exact: true })).toBeVisible();
	});

	it('renders plain lyrics when no timed lines are available', async () => {
		await render(LyricsPanel, {
			open: true,
			lyricsText: 'Plain first line\nPlain second line',
			lines: [
				{ text: 'Plain first line', start_seconds: null },
				{ text: 'Plain second line', start_seconds: null }
			],
			isSynced: false,
			onclose: vi.fn()
		});

		await expect.element(page.getByText(/Plain first line/)).toBeVisible();
		await expect.element(page.getByText('Synced', { exact: true })).not.toBeInTheDocument();
	});
});
