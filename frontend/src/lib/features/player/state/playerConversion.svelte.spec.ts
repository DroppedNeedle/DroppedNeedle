import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';

import QueueList from '$lib/features/player/components/QueueList.svelte';
import { setAudioElement } from '$lib/features/player/playback/audioElement';
import type { QueueItem, SourceType } from '$lib/features/player/playback/types';
import { queryClient } from '$lib/queries/QueryClient';
import { toastStore } from '$lib/stores/toast';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { playerStore } from './player.svelte';

const MEDIA_ERR_NETWORK = 2;
const MEDIA_ERR_SRC_NOT_SUPPORTED = 4;
const ALAC = 'audio/mp4; codecs="alac"';

// The real store and source over an element whose loads fail with the code
// `fails` gives, or play. `play` comes first, then the load's own result, as
// in the browser.
function fakeElement(fails: (url: string) => number | null) {
	const element = new Audio();
	let src = '';
	let error: { code: number } | null = null;
	const loads: string[] = [];
	Object.defineProperty(element, 'src', { get: () => src, set: (url: string) => (src = url) });
	Object.defineProperty(element, 'error', { get: () => error });
	element.load = () => {
		if (!src) return;
		loads.push(src);
		const code = fails(src);
		error = code === null ? null : { code };
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

// The server: whether it converts, and the session reports a media server track sends.
function server(conversion: 'on' | 'off') {
	return fakeApi({
		'GET /api/v3/playback/conversion': { conversion },
		'POST /api/v3/playback/start': { accepted: true, session: 's' },
		'POST /api/v3/playback/progress': { accepted: true },
		'POST /api/v3/playback/stop': { accepted: true, scrobbled: false }
	});
}

function item(id: string, sourceType: SourceType, format: string | undefined): QueueItem {
	return {
		trackSourceId: id,
		trackName: id,
		artistName: 'Artist',
		trackNumber: 1,
		albumId: 'album',
		albumName: 'Album',
		coverUrl: null,
		sourceType,
		format,
		duration: 200,
		availableSources: [sourceType],
		sourceIds: { [sourceType]: id }
	};
}

describe('a media server track the browser cannot play', () => {
	beforeEach(async () => {
		// Playback starts from a person's click; the audio engine waits for one.
		const button = document.createElement('button');
		button.textContent = 'play';
		document.body.append(button);
		await userEvent.click(button);
		button.remove();
		queryClient.clear();
		vi.spyOn(HTMLMediaElement.prototype, 'canPlayType').mockImplementation((type) =>
			type === ALAC ? '' : 'probably'
		);
		vi.spyOn(toastStore, 'notify').mockImplementation(() => {});
	});

	afterEach(() => {
		playerStore.stop();
		queryClient.clear();
		vi.restoreAllMocks();
	});

	it('asks the server to convert it and seeks in the conversion', async () => {
		server('on');
		const { loads } = fakeElement(() => null);

		playerStore.playQueue([item('jf-alac', 'jellyfin', 'alac')]);
		await expect.poll(() => playerStore.playbackState).toBe('playing');

		expect(loads).toEqual(['/api/v3/stream/jellyfin/jf-alac?format=flac']);
		expect(playerStore.converting).toBe(true);
		playerStore.seekTo(42);
		expect(loads.at(-1)).toBe('/api/v3/stream/jellyfin/jf-alac?format=flac&start_seconds=42');
		expect(toastStore.notify).not.toHaveBeenCalled();
	});

	it('with converting off, says why once, marks it and moves on without asking', async () => {
		server('off');
		const { loads } = fakeElement(() => null);
		await renderWithQuery(QueueList, { oncleared: () => {} }, { client: queryClient });

		playerStore.playQueue([
			item('nd-alac', 'navidrome', 'alac'),
			item('next', 'local', 'mp3'),
			item('later-alac', 'jellyfin', 'alac')
		]);
		await expect.poll(() => playerStore.currentIndex, { timeout: 4000 }).toBe(1);
		await expect.poll(() => playerStore.playbackState).toBe('playing');

		expect(loads).toEqual(['/api/v3/stream/local/next']);
		expect(toastStore.notify).toHaveBeenCalledTimes(1);
		expect(toastStore.notify).toHaveBeenCalledWith(
			'Skipped "nd-alac": this browser can\'t play ALAC',
			'warning'
		);
		// The skipped track and the one not reached yet are both marked.
		await expect.element(page.getByText("Can't play").nth(1)).toBeVisible();
		expect(playerStore.unplayableReasonFor(playerStore.queue[2])).toBe('converting_off');
	});

	it('retries a fallback source that will not decode as a conversion', async () => {
		server('on');
		const { loads } = fakeElement((url) => {
			if (url.includes('/jellyfin/')) return MEDIA_ERR_NETWORK;
			return url.includes('format=') ? null : MEDIA_ERR_SRC_NOT_SUPPORTED;
		});
		const both: QueueItem = {
			...item('jf-flac', 'jellyfin', 'flac'),
			availableSources: ['jellyfin', 'local'],
			sourceIds: { jellyfin: 'jf-flac', local: 'local-copy' },
			sourceFormats: { jellyfin: 'flac' }
		};

		playerStore.playQueue([both]);
		await expect.poll(() => playerStore.playbackState).toBe('playing');

		expect(loads).toEqual([
			'/api/v3/stream/jellyfin/jf-flac',
			'/api/v3/stream/jellyfin/jf-flac',
			'/api/v3/stream/local/local-copy',
			'/api/v3/stream/local/local-copy?format=flac'
		]);
		expect(toastStore.notify).not.toHaveBeenCalled();
	});
});
