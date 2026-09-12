import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

vi.mock('$env/dynamic/public', () => ({ env: { PUBLIC_API_URL: '' } }));

// A mutable stand-in for the sampler store so tests control what the widget sees.
const sampler = vi.hoisted(() => {
	const s = {
		status: 'playing' as string,
		currentEntry: {
			key: 'rg-1',
			kind: 'album' as const,
			artist: 'Slowdive',
			title: 'Souvlaki',
			albumMbid: 'rg-1',
			artistMbid: 'a-1',
			coverUrl: null as string | null
		},
		provider: 'deezer' as string | null,
		progress: 0.5,
		volume: 0.7,
		isStation: false,
		stationPosition: { index: 0, total: 1 },
		hasNext: false,
		togglePlay: vi.fn(),
		next: vi.fn(),
		stop: vi.fn(),
		setVolume: vi.fn()
	};
	return s;
});

vi.mock('$lib/stores/deckSampler.svelte', () => ({ deckSampler: sampler }));

const player = vi.hoisted(() => ({ isPlaying: false, isPlayerVisible: true }));
vi.mock('$lib/stores/player.svelte', () => ({ playerStore: player }));

import PreviewWidget from './PreviewWidget.svelte';

describe('PreviewWidget', () => {
	it('shows the sampled album and artist, and wires the controls', async () => {
		const { unmount } = await render(PreviewWidget);

		await expect.element(page.getByText('Souvlaki')).toBeVisible();
		await expect.element(page.getByText('Slowdive', { exact: false })).toBeVisible();
		await expect.element(page.getByText('Now sampling')).toBeVisible();

		await page.getByRole('button', { name: /pause preview/i }).click();
		expect(sampler.togglePlay).toHaveBeenCalled();

		await page.getByRole('button', { name: /close preview/i }).click();
		expect(sampler.stop).toHaveBeenCalled();

		await unmount();
	});

	it('is hidden when the sampler is idle', async () => {
		sampler.status = 'idle';
		const { container, unmount } = await render(PreviewWidget);
		expect(container.querySelector('.preview-widget')).toBeNull();
		sampler.status = 'playing';
		await unmount();
	});
});
