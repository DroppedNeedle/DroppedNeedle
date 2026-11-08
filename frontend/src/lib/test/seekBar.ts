import { expect, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';

import { setAudioElement } from '$lib/features/player/playback/audioElement';
import { playerStore } from '$lib/features/player/state/player.svelte';
import { queryClient } from '$lib/queries/QueryClient';

/**
 * Start a 200-second local track on an element that plays at once and keeps
 * the position it is given, and spy on the store's seeks. Pair it with
 * `playerStore.stop()` and `vi.restoreAllMocks()` after each test.
 */
export async function playSeekableTrack(): Promise<void> {
	// Playback starts from a person's click; the audio engine waits for one.
	const button = document.createElement('button');
	button.textContent = 'play';
	document.body.append(button);
	await userEvent.click(button);
	button.remove();
	queryClient.clear();
	await page.viewport(1280, 800);

	const element = new Audio();
	let src = '';
	let currentTime = 0;
	Object.defineProperty(element, 'src', { get: () => src, set: (url: string) => (src = url) });
	Object.defineProperty(element, 'currentTime', {
		get: () => currentTime,
		set: (t: number) => (currentTime = t)
	});
	Object.defineProperty(element, 'duration', { get: () => 200 });
	element.load = () => {
		if (src) setTimeout(() => element.dispatchEvent(new Event('canplay')), 1);
	};
	element.play = async () => {
		element.dispatchEvent(new Event('play'));
		setTimeout(() => element.dispatchEvent(new Event('playing')), 2);
	};
	setAudioElement(element);

	playerStore.playQueue([
		{
			trackSourceId: 't1',
			trackName: 'Track',
			artistName: 'Artist',
			trackNumber: 1,
			albumId: 'album',
			albumName: 'Album',
			coverUrl: null,
			sourceType: 'local',
			format: 'mp3',
			duration: 200,
			availableSources: ['local'],
			sourceIds: { local: 't1' }
		}
	]);
	await expect.poll(() => playerStore.playbackState).toBe('playing');
	vi.spyOn(playerStore, 'seekTo');
}

/** The rendered seek bar, once it can be used. */
export async function seekBar(): Promise<HTMLInputElement> {
	const bar = page.getByRole('slider', { name: 'Seek' });
	await expect.element(bar).toBeEnabled();
	return bar.element() as HTMLInputElement;
}

/** Press near the bar's start, drag to `ratio` of its width and let go there. */
export async function dragSeekBar(bar: HTMLInputElement, ratio: number): Promise<void> {
	const { width, height } = bar.getBoundingClientRect();
	await userEvent.dragAndDrop(bar, bar, {
		sourcePosition: { x: 2, y: height / 2 },
		targetPosition: { x: width * ratio, y: height / 2 }
	} as never);
}

/** The store's last seek, once there is one: within a thumb's width of `seconds`. */
export async function expectSeekNear(seconds: number): Promise<void> {
	await expect.poll(() => vi.mocked(playerStore.seekTo).mock.calls.length).toBeGreaterThan(0);
	const target = vi.mocked(playerStore.seekTo).mock.calls.at(-1)?.[0] ?? 0;
	expect(Math.abs(target - seconds)).toBeLessThan(15);
}
