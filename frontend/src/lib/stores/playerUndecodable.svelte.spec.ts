import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { userEvent } from 'vitest/browser';

import { setAudioElement } from '$lib/player/audioElement';
import type { QueueItem } from '$lib/player/types';
import { toastStore } from '$lib/stores/toast';
import { playerStore } from './player.svelte';

const MEDIA_ERR_SRC_NOT_SUPPORTED = 4;
const ALAC = 'audio/mp4; codecs="alac"';
const OGG_VORBIS = 'audio/ogg; codecs="vorbis"';

// The real store and source over an element that plays only what `plays` says.
// In the browser's order: `play` first, then the load's own result, so a file
// that cannot decode still looks asked-to-play before it fails.
function fakeElement(plays: (url: string) => boolean) {
	const element = new Audio();
	let src = '';
	let error: { code: number } | null = null;
	const loads: string[] = [];
	Object.defineProperty(element, 'src', { get: () => src, set: (url: string) => (src = url) });
	Object.defineProperty(element, 'error', { get: () => error });
	element.load = () => {
		if (!src) return;
		loads.push(src);
		error = plays(src) ? null : { code: MEDIA_ERR_SRC_NOT_SUPPORTED };
		setTimeout(() => {
			element.dispatchEvent(new Event(error ? 'error' : 'canplay'));
		}, 1);
	};
	element.play = async () => {
		element.dispatchEvent(new Event('play'));
		if (!error) setTimeout(() => element.dispatchEvent(new Event('playing')), 2);
	};
	setAudioElement(element);
	return { element, loads };
}

function item(id: string, format: string): QueueItem {
	return {
		trackSourceId: id,
		trackName: id,
		artistName: 'Artist',
		trackNumber: 1,
		albumId: 'album',
		albumName: 'Album',
		coverUrl: null,
		sourceType: 'local',
		format,
		duration: 200,
		availableSources: ['local'],
		sourceIds: { local: id }
	};
}

describe('a track the browser cannot play', () => {
	let refused: string[];

	beforeEach(async () => {
		// Playback starts from a person's click; the audio engine waits for one.
		const button = document.createElement('button');
		button.textContent = 'play';
		document.body.append(button);
		await userEvent.click(button);
		button.remove();
		refused = [ALAC];
		vi.spyOn(HTMLMediaElement.prototype, 'canPlayType').mockImplementation((type) =>
			refused.includes(type) ? '' : 'probably'
		);
		vi.spyOn(toastStore, 'notify').mockImplementation(() => {});
	});

	afterEach(() => {
		playerStore.stop();
		vi.restoreAllMocks();
	});

	it('plays a lossless file as FLAC, with its length and seeking', async () => {
		const { element, loads } = fakeElement(() => true);

		playerStore.playQueue([item('alac-track', 'alac')]);
		await expect.poll(() => playerStore.playbackState).toBe('playing');

		expect(loads).toEqual(['/api/v3/stream/local/alac-track?format=flac']);
		// The pipe has no length of its own; the track's known length stands in.
		expect(playerStore.currentSource?.getDuration()).toBe(200);

		playerStore.seekTo(42);
		expect(loads.at(-1)).toBe('/api/v3/stream/local/alac-track?format=flac&start_seconds=42');
		expect(playerStore.currentSource?.getCurrentTime()).toBe(element.currentTime + 42);
		expect(toastStore.notify).not.toHaveBeenCalled();
	});

	it('plays a lossy file as a lossy format the browser takes', async () => {
		const { loads } = fakeElement(() => true);
		refused = [OGG_VORBIS];

		playerStore.playQueue([item('vorbis-track', 'ogg')]);
		await expect.poll(() => playerStore.playbackState).toBe('playing');
		expect(loads).toEqual(['/api/v3/stream/local/vorbis-track?format=opus']);
	});

	it('turns an original that will not decode into a transcode, without a word', async () => {
		const { loads } = fakeElement((url) => url.includes('format='));

		playerStore.playQueue([item('labelled-m4a', 'm4a')]);
		await expect.poll(() => playerStore.playbackState).toBe('playing');

		expect(loads).toEqual([
			'/api/v3/stream/local/labelled-m4a',
			'/api/v3/stream/local/labelled-m4a?format=flac'
		]);
		expect(toastStore.notify).not.toHaveBeenCalled();
	});

	it('skips a failing one with one message and no second try', async () => {
		const { loads } = fakeElement((url) => !url.includes('/broken'));

		playerStore.playQueue([item('broken', 'alac'), item('next', 'mp3')]);
		await expect.poll(() => playerStore.currentIndex, { timeout: 4000 }).toBe(1);
		await expect.poll(() => playerStore.playbackState).toBe('playing');

		expect(loads).toEqual(['/api/v3/stream/local/broken?format=flac', '/api/v3/stream/local/next']);
		expect(toastStore.notify).toHaveBeenCalledTimes(1);
		expect(toastStore.notify).toHaveBeenCalledWith('Skipped "broken": can\'t play it', 'warning');
	});
});
