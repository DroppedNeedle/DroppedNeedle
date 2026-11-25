import { page } from 'vitest/browser';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { toAuthUser } from '$lib/features/auth/queries/types';
import type { SetPick } from '$lib/features/discover/queries/week.svelte';
import { deckSampler } from '$lib/features/discover/state/deckSampler.svelte';
import { queryClient } from '$lib/queries/QueryClient';
import { startLockScreen } from '$lib/shell/lockScreen';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import PickSampleTestHarness from './PickSampleTestHarness.svelte';

const PREVIEW = 'GET /api/v3/discover/album-preview';
const MBID = '0b1f2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d';

const PICK: SetPick = {
	pick_no: 3,
	album: {
		title: 'Mothership',
		musicbrainz_id: MBID,
		artist_name: 'Dance Gavin Dance',
		in_library: false,
		requested: false
	},
	reason: { kind: 'your_artists', line: 'Closest artist to Circa Survive.', artists: [] },
	state: 'open',
	heard: false,
	taste_no: 0
};

/** An element that plays at once and loads nothing: no clip is fetched. */
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
	load() {}
	removeAttribute() {}
}

type LockPlayer = Parameters<typeof startLockScreen>[0];

/** A player with `nowPlaying` loaded, or none. */
function player(nowPlaying: LockPlayer['nowPlaying']): LockPlayer {
	return {
		nowPlaying,
		playbackState: nowPlaying ? 'paused' : 'idle',
		progress: 0,
		duration: nowPlaying ? 200 : 0,
		isSeekable: !!nowPlaying,
		play: () => {},
		pause: () => {},
		nextTrack: () => {},
		previousTrack: () => {},
		seekTo: () => {}
	};
}

const realAudio = window.Audio;
let stopLockScreen = () => {};

beforeEach(() => {
	window.Audio = SilentAudio as unknown as typeof Audio;
	fakeApi({
		[PREVIEW]: () => ({
			provider: 'itunes',
			tracks: [
				{
					title: 'Chucky vs. the Giant Tortoise',
					artist_name: 'Dance Gavin Dance',
					preview_url: 'https://audio-ssl.itunes.apple.com/clip.m4a',
					duration_s: 30,
					position: 1,
					role: 'opener',
					source: 'itunes'
				}
			]
		})
	});
	authStore.setUser(
		toAuthUser({
			id: 'u1',
			display_name: 'Ada',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'ada',
			username_display: 'ada'
		})
	);
});

afterEach(() => {
	deckSampler.stop();
	stopLockScreen();
	window.Audio = realAudio;
	vi.restoreAllMocks();
});

describe('a pick sampled on the counter', () => {
	it('toggles by name, says where the clip is from, and shows it on the lock screen', async () => {
		stopLockScreen = startLockScreen(player(null));
		await renderWithQuery(PickSampleTestHarness, { pick: PICK }, { client: queryClient });

		await page.getByRole('button', { name: 'Play a sample of Mothership' }).click();
		const stop = page.getByRole('button', { name: 'Stop the sample' });
		await expect.element(stop).toHaveAttribute('aria-pressed', 'true');
		await expect.element(page.getByText('Sample - iTunes')).toBeVisible();
		await expect.element(page.getByText('Chucky vs. the Giant Tortoise')).toBeVisible();
		await expect.poll(() => navigator.mediaSession.metadata?.album).toBe('Mothership');
		expect(navigator.mediaSession.metadata?.artist).toBe('Dance Gavin Dance');
		expect(navigator.mediaSession.metadata?.title).toBe('Chucky vs. the Giant Tortoise');

		await stop.click();
		await expect
			.element(page.getByRole('button', { name: 'Play a sample of Mothership' }))
			.toHaveAttribute('aria-pressed', 'false');
		await expect.element(page.getByText('Nothing playing')).toBeVisible();
		await expect.poll(() => navigator.mediaSession.metadata).toBeNull();
	});

	it('gives the lock screen back to the player, seeking included, when the sample stops', async () => {
		const handlers = vi.spyOn(navigator.mediaSession, 'setActionHandler');
		const lastSeek = () =>
			handlers.mock.calls.filter(([action]) => action === 'seekto').at(-1)?.[1] ?? null;
		stopLockScreen = startLockScreen(
			player({
				albumId: MBID,
				albumName: 'Downtown Battle Mountain',
				artistName: 'Dance Gavin Dance',
				coverUrl: null,
				sourceType: 'local',
				trackName: 'Lemon Meringue Tie'
			})
		);
		await expect.poll(() => navigator.mediaSession.metadata?.title).toBe('Lemon Meringue Tie');
		await renderWithQuery(PickSampleTestHarness, { pick: PICK }, { client: queryClient });

		await page.getByRole('button', { name: 'Play a sample of Mothership' }).click();
		await expect
			.poll(() => navigator.mediaSession.metadata?.title)
			.toBe('Chucky vs. the Giant Tortoise');
		expect(navigator.mediaSession.metadata?.album).toBe('Mothership');
		await expect.poll(lastSeek).toBeNull();

		await page.getByRole('button', { name: 'Stop the sample' }).click();
		await expect.poll(() => navigator.mediaSession.metadata?.title).toBe('Lemon Meringue Tie');
		expect(navigator.mediaSession.metadata?.album).toBe('Downtown Battle Mountain');
		await expect.poll(lastSeek).toBeTypeOf('function');
	});
});
