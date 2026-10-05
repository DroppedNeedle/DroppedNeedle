import { page } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';
import QueueDrawer from './QueueDrawer.svelte';

vi.mock('$lib/player/createSource', () => ({
	createPlaybackSource: vi.fn(() => ({
		type: 'local' as const,
		load: vi.fn().mockResolvedValue(undefined),
		play: vi.fn(),
		pause: vi.fn(),
		seekTo: vi.fn(),
		setVolume: vi.fn(),
		getCurrentTime: vi.fn(() => 0),
		getDuration: vi.fn(() => 180),
		isSeekable: vi.fn(() => true),
		destroy: vi.fn(),
		onStateChange: vi.fn(),
		onReady: vi.fn(),
		onError: vi.fn(),
		onProgress: vi.fn()
	}))
}));

import { playerStore } from '$lib/stores/player.svelte';

async function renderDrawer(open: boolean, onclose: () => void) {
	return await render(QueueDrawer, {
		props: { open, onclose }
	} as Parameters<typeof render<typeof QueueDrawer>>[1]);
}

describe('QueueDrawer.svelte', () => {
	beforeEach(() => {
		playerStore.stop();
	});

	it('clears upcoming tracks on clear click but keeps current track', async () => {
		playerStore.playQueue([
			{
				trackSourceId: 'v1',
				trackName: 'Track A',
				artistName: 'Artist',
				trackNumber: 1,
				albumId: 'a1',
				albumName: 'Album',
				coverUrl: null,
				sourceType: 'local',
				streamUrl: 'http://test/1.mp3'
			},
			{
				trackSourceId: 'v2',
				trackName: 'Track B',
				artistName: 'Artist',
				trackNumber: 2,
				albumId: 'a1',
				albumName: 'Album',
				coverUrl: null,
				sourceType: 'local',
				streamUrl: 'http://test/2.mp3'
			}
		]);
		const onclose = vi.fn();
		await renderDrawer(true, onclose);
		const clearBtn = page.getByText('Clear');
		await expect.element(clearBtn).toBeVisible();
		await clearBtn.click();
		await expect.element(page.getByText('Queue')).not.toBeInTheDocument();
		expect(playerStore.queue).toHaveLength(1);
		expect(playerStore.queue[0].trackSourceId).toBe('v1');
		expect(onclose).toHaveBeenCalledTimes(1);
	});

	it('only allows reordering upcoming tracks', async () => {
		playerStore.playQueue(
			[
				{
					trackSourceId: 'v1',
					trackName: 'Track A',
					artistName: 'Artist',
					trackNumber: 1,
					albumId: 'a1',
					albumName: 'Album',
					coverUrl: null,
					sourceType: 'local',
					streamUrl: 'http://test/1.mp3'
				},
				{
					trackSourceId: 'v2',
					trackName: 'Track B',
					artistName: 'Artist',
					trackNumber: 2,
					albumId: 'a1',
					albumName: 'Album',
					coverUrl: null,
					sourceType: 'local',
					streamUrl: 'http://test/2.mp3'
				},
				{
					trackSourceId: 'v3',
					trackName: 'Track C',
					artistName: 'Artist',
					trackNumber: 3,
					albumId: 'a1',
					albumName: 'Album',
					coverUrl: null,
					sourceType: 'local',
					streamUrl: 'http://test/3.mp3'
				}
			],
			1
		);
		const onclose = vi.fn();
		const view = await renderDrawer(true, onclose);
		await expect.element(page.getByText('Track C')).toBeVisible();

		const enabledHandles = view.container.querySelectorAll(
			'button[aria-label="Drag to reorder"]:not([disabled])'
		);
		expect(enabledHandles).toHaveLength(1);
	});
});
