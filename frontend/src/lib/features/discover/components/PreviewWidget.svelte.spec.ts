import { page } from 'vitest/browser';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { queryClient } from '$lib/queries/QueryClient';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { deckSampler, type SampleEntry } from '$lib/features/discover/state/deckSampler.svelte';
import { pickAnswers } from '$lib/features/discover/state/pickAnswers.svelte';
import PreviewWidget from './PreviewWidget.svelte';

/** An element that plays at once and never ends, so the week stays on its first clip. */
class SilentAudio extends EventTarget {
	src = '';
	preload = '';
	muted = false;
	volume = 1;
	currentTime = 0;
	duration = 30;
	ended = false;
	play() {
		return Promise.resolve();
	}
	pause() {}
	removeAttribute(name: string) {
		if (name === 'src') this.src = '';
	}
	load() {}
}

const realAudio = window.Audio;

beforeEach(() => {
	(window as unknown as { Audio: typeof SilentAudio }).Audio = SilentAudio;
});

afterEach(() => {
	deckSampler.stop();
	window.Audio = realAudio;
});

const week: SampleEntry[] = [1, 2].map((n) => ({
	key: `rg-${n}`,
	kind: 'album',
	artist: `Artist ${n}`,
	title: `Album ${n}`,
	mode: 'audition',
	pick: 'weekly_set'
}));

describe('the floating sample player', () => {
	it('shows the week still playing once the Discover page has gone', async () => {
		fakeApi({
			'GET /api/v3/discover/album-preview': () => ({
				provider: 'deezer',
				tracks: [
					{
						title: 'Opener',
						artist_name: 'Artist 1',
						preview_url: 'https://cdn.example/1.mp3',
						duration_s: 30,
						position: 1,
						role: 'opener',
						source: 'deezer'
					}
				]
			})
		});
		await renderWithQuery(PreviewWidget, {}, { client: queryClient });
		const leave = pickAnswers.register({ open: () => true });

		deckSampler.startStation('Week 47', week, 'week');
		await expect.poll(() => deckSampler.status).toBe('playing');
		await expect
			.element(page.getByRole('region', { name: 'Album preview' }))
			.not.toBeInTheDocument();

		leave();

		await expect.element(page.getByRole('region', { name: 'Album preview' })).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Pause preview' })).toBeVisible();
	});
});
