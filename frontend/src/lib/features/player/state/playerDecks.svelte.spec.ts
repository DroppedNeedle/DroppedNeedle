import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { userEvent } from 'vitest/browser';

import { setAudioDecks } from '$lib/features/player/playback/audioElement';
import { AudioEngine } from '$lib/features/player/playback/audioEngine';
import type { QueueItem } from '$lib/features/player/playback/types';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { fakeApi, type FakeApi } from '$lib/test/api';
import { crossfade } from './crossfade.svelte';
import { playerStore } from './player.svelte';

const MEDIA_ERR_NETWORK = 2;

interface FakeDeck {
	element: HTMLAudioElement;
	/** What it was asked to play, in order. */
	plays: string[];
	playing(): boolean;
	/** Moves the play head and says so, as the element does a few times a second. */
	at(seconds: number): void;
	fail(): void;
}

// A deck that loads at once, holds whatever position it is given and plays
// when asked, 200 seconds long. Emptying it stops it, as on a real element.
function fakeDeck(): FakeDeck {
	const element = new Audio();
	let src = '';
	let time = 0;
	let paused = true;
	let ready = 0;
	let error: { code: number } | null = null;
	const plays: string[] = [];
	Object.defineProperty(element, 'src', {
		get: () => src,
		set: (url: string) => {
			src = url;
			paused = true;
			ready = 0;
			time = 0;
			error = null;
		}
	});
	Object.defineProperty(element, 'currentTime', {
		get: () => time,
		set: (t: number) => (time = t)
	});
	Object.defineProperty(element, 'duration', { get: () => (src ? 200 : NaN) });
	Object.defineProperty(element, 'paused', { get: () => paused });
	Object.defineProperty(element, 'readyState', { get: () => ready });
	Object.defineProperty(element, 'error', { get: () => error });
	element.load = () => {
		const loading = src;
		if (!loading) return;
		setTimeout(() => {
			if (src !== loading) return;
			ready = 4;
			element.dispatchEvent(new Event('canplay'));
		}, 1);
	};
	element.play = async () => {
		const playing = src;
		plays.push(playing);
		paused = false;
		element.dispatchEvent(new Event('play'));
		setTimeout(() => {
			if (src === playing && !paused) element.dispatchEvent(new Event('playing'));
		}, 2);
	};
	element.pause = () => {
		if (paused) return;
		paused = true;
		element.dispatchEvent(new Event('pause'));
	};
	return {
		element,
		plays,
		playing: () => !paused && src !== '',
		at(seconds) {
			time = seconds;
			element.dispatchEvent(new Event('timeupdate'));
		},
		fail() {
			error = { code: MEDIA_ERR_NETWORK };
			paused = true;
			element.dispatchEvent(new Event('error'));
		}
	};
}

// Jellyfin tracks, so each play opens a session the server hears about.
function track(id: string, albumId: string, trackNumber: number): QueueItem {
	return {
		trackSourceId: id,
		trackName: id,
		artistName: 'Artist',
		trackNumber,
		albumId,
		albumName: albumId,
		coverUrl: null,
		sourceType: 'jellyfin',
		format: 'mp3',
		duration: 200,
		availableSources: ['jellyfin'],
		sourceIds: { jellyfin: id }
	};
}

function playingDecks(decks: FakeDeck[]): string[] {
	return decks.filter((deck) => deck.playing()).map((deck) => deck.element.src);
}

const stream = (id: string) => `/api/v3/stream/jellyfin/${id}`;

// A library track, which the player levels.
function local(id: string, albumId: string): QueueItem {
	return {
		...track(id, albumId, 1),
		sourceType: 'local',
		format: 'flac',
		availableSources: ['local'],
		sourceIds: { local: id }
	};
}

const isDown2dB = (gain: number) => Math.abs(gain - 10 ** (-2 / 20)) < 1e-9;

/** Every level the engine is asked for, as [deck, gain], in order. */
function recordLevels(): number[][] {
	const levels: number[][] = [];
	const setLevel = AudioEngine.prototype.setLevel;
	vi.spyOn(AudioEngine.prototype, 'setLevel').mockImplementation(function (
		this: AudioEngine,
		deck: number,
		gain: number,
		ramp?: number
	) {
		levels.push([deck, gain]);
		setLevel.call(this, deck, gain, ramp);
	});
	return levels;
}

describe('the next track on the second deck', () => {
	let server: FakeApi;
	let a: FakeDeck;
	let b: FakeDeck;

	beforeEach(async () => {
		// Playback starts from a person's click; the audio engine waits for one.
		const button = document.createElement('button');
		button.textContent = 'play';
		document.body.append(button);
		await userEvent.click(button);
		button.remove();
		queryClient.clear();
		authStore.setUser({
			id: 'deck-user',
			display_name: 'Listener',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'listener',
			username_display: 'Listener',
			providers: ['local']
		});
		server = fakeApi({
			'POST /api/v3/playback/start': { accepted: true, session: 's' },
			'POST /api/v3/playback/progress': { accepted: true },
			'POST /api/v3/playback/stop': { accepted: true, scrobbled: false },
			'HEAD /api/v3/stream/{source}/{*key}': undefined
		});
		vi.spyOn(HTMLMediaElement.prototype, 'canPlayType').mockReturnValue('probably');
		a = fakeDeck();
		b = fakeDeck();
		setAudioDecks([a.element, b.element]);
	});

	afterEach(() => {
		playerStore.stop();
		crossfade.setSeconds(0);
		localStorage.removeItem('droppedneedle_player_crossfade:deck-user');
		queryClient.clear();
		vi.restoreAllMocks();
	});

	/** Plays `items` from the first, then loads the second ahead near the first's end. */
	async function playUntilPreloaded(items: QueueItem[]): Promise<void> {
		playerStore.playQueue(items);
		await expect.poll(() => playerStore.playbackState).toBe('playing');
		a.at(180);
		await expect.poll(() => b.element.readyState).toBe(4);
		expect(b.element.src).toBe(stream(items[1].trackSourceId));
		expect(b.plays).toEqual([]);
	}

	it('starts the next track before the current one ends, with one stop and one start', async () => {
		await playUntilPreloaded([track('one', 'album', 1), track('two', 'album', 2)]);
		await expect.poll(() => server.calls('POST /api/v3/playback/start')).toHaveLength(1);
		const aPlayingAtB: boolean[] = [];
		const playB = b.element.play;
		b.element.play = () => {
			aPlayingAtB.push(a.playing());
			return playB();
		};

		a.at(199.99);

		// B starts from the timer while A still plays: A's end never came.
		await expect.poll(() => b.plays).toEqual([stream('two')]);
		expect(aPlayingAtB).toEqual([true]);
		expect(playerStore.nowPlaying?.trackName).toBe('two');
		expect(playerStore.playbackState).toBe('playing');
		// A's join ends it, leaving B alone.
		await expect.poll(() => playingDecks([a, b])).toEqual([stream('two')]);
		await expect.poll(() => server.calls('POST /api/v3/playback/start')).toHaveLength(2);
		const stops = server.calls('POST /api/v3/playback/stop');
		expect(stops).toHaveLength(1);
		expect(stops[0].body).toMatchObject({ track_id: 'one', position_ms: 200_000 });
		expect(server.calls('POST /api/v3/playback/start')[1].body).toMatchObject({
			track_id: 'two'
		});
		expect(a.plays).toEqual([stream('one')]);
	});

	it('leaves only the right deck playing after a skip, a pause or an error mid-fade', async () => {
		const queue = () => [
			track('one', 'first', 1),
			track('two', 'second', 1),
			track('three', 'third', 1)
		];
		const crossfading = async () => {
			crossfade.setSeconds(6);
			await playUntilPreloaded(queue());
			a.at(194);
			await expect.poll(() => b.plays).toEqual([stream('two')]);
			expect(playingDecks([a, b]).sort()).toEqual([stream('one'), stream('two')]);
		};

		{
			// A skip plays the track after the one the bar shows.
			await crossfading();
			playerStore.nextTrack();
			await expect.poll(() => playingDecks([a, b])).toEqual([stream('three')]);
			expect(playerStore.nowPlaying?.trackName).toBe('three');
			playerStore.stop();
		}

		{
			// A pause holds the incoming track; playing again plays only that one.
			a = fakeDeck();
			b = fakeDeck();
			setAudioDecks([a.element, b.element]);
			await crossfading();
			playerStore.pause();
			await expect.poll(() => playingDecks([a, b])).toEqual([]);
			expect(a.element.src).toBe('');
			playerStore.play();
			await expect.poll(() => playingDecks([a, b])).toEqual([stream('two')]);
			playerStore.stop();
		}

		{
			// The incoming track failing loads it again, alone.
			a = fakeDeck();
			b = fakeDeck();
			setAudioDecks([a.element, b.element]);
			await crossfading();
			b.fail();
			await expect.poll(() => playingDecks([a, b])).toEqual([stream('two')]);
			expect(playerStore.nowPlaying?.trackName).toBe('two');
		}
	});

	it('starts nothing early while the current track is paused, even sought to its end', async () => {
		await playUntilPreloaded([track('one', 'album', 1), track('two', 'album', 2)]);

		playerStore.pause();
		a.at(199.99);
		// A start would be armed for at once; give it the chance.
		await new Promise((resolve) => setTimeout(resolve, 50));
		expect(b.plays).toEqual([]);
		expect(playerStore.nowPlaying?.trackName).toBe('one');

		playerStore.play();
		a.at(199.99);
		await expect.poll(() => b.plays).toEqual([stream('two')]);
	});

	it('loads a track the browser refused at the join again on the deck that played', async () => {
		// A transcode the refusal must not mark as failed.
		vi.mocked(HTMLMediaElement.prototype.canPlayType).mockImplementation((type) =>
			type.includes('alac') ? '' : 'probably'
		);
		server.use({ 'GET /api/v3/playback/conversion': { conversion: 'on' } });
		const notify = vi.spyOn(toastStore, 'notify').mockImplementation(() => {});
		b.element.play = () => Promise.reject(new DOMException('refused', 'NotAllowedError'));
		const two = { ...track('two', 'album', 2), format: 'alac' };
		playerStore.playQueue([track('one', 'album', 1), two]);
		await expect.poll(() => playerStore.playbackState).toBe('playing');
		a.at(180);
		await expect.poll(() => b.element.readyState).toBe(4);

		a.at(199.99);

		await expect.poll(() => a.plays).toEqual([stream('one'), `${stream('two')}?format=flac`]);
		await expect.poll(() => playerStore.playbackState).toBe('playing');
		expect(playerStore.nowPlaying?.trackName).toBe('two');
		expect(playerStore.unplayableReasonFor(playerStore.queue[playerStore.currentIndex])).toBe(null);
		expect(notify).not.toHaveBeenCalled();
	});

	it('crossfades on the length the element reads when the catalog gives another', async () => {
		crossfade.setSeconds(6);
		const one = { ...track('one', 'first', 1), duration: 197 };
		await playUntilPreloaded([one, track('two', 'second', 1)]);

		a.at(194);

		await expect.poll(() => b.plays).toEqual([stream('two')]);
	});

	it('drops the preload when the queue changes what plays next', async () => {
		await playUntilPreloaded([track('one', 'album', 1), track('two', 'album', 2)]);

		playerStore.playNext(track('other', 'album', 5));

		await expect.poll(() => b.element.src).toBe('');
		a.at(181);
		await expect.poll(() => b.element.src).toBe(stream('other'));
	});

	it("sets the next song's level on its deck before it plays", async () => {
		server.use({
			'GET /api/v3/library/tracks/{id}/loudness': ({ params }) => ({
				track_gain_db: params.id === 'two' ? -6 : null,
				track_peak: params.id === 'two' ? 0.5 : null,
				album_gain_db: null,
				album_peak: null
			})
		});
		const levels = recordLevels();
		const levelsAtPlay: number[][][] = [];
		const playB = b.element.play;
		b.element.play = () => {
			levelsAtPlay.push([...levels]);
			return playB();
		};
		playerStore.playQueue([local('one', 'first'), local('two', 'second')]);
		await expect.poll(() => playerStore.playbackState).toBe('playing');
		a.at(180);
		await expect.poll(() => b.element.readyState).toBe(4);

		a.at(199.99);

		await expect.poll(() => b.plays).toEqual(['/api/v3/stream/local/two']);
		// -6 dB to the reference, 4 dB above it: 2 dB down on B's deck, set before B played.
		expect(levelsAtPlay).toHaveLength(1);
		expect(levelsAtPlay[0].some(([deck, gain]) => deck === 1 && isDown2dB(gain))).toBe(true);
	});

	it('asks for the stream before the loudness answers, and plays once the level is set', async () => {
		let answer = () => {};
		const answered = new Promise<void>((resolve) => (answer = resolve));
		const events: string[] = [];
		server.use({
			'GET /api/v3/library/tracks/{id}/loudness': async () => {
				await answered;
				events.push('loudness');
				return { track_gain_db: -6, track_peak: 0.5, album_gain_db: null, album_peak: null };
			}
		});
		const levels = recordLevels();
		const loadA = a.element.load;
		a.element.load = () => {
			events.push('load');
			loadA.call(a.element);
		};
		const playA = a.element.play;
		a.element.play = () => {
			events.push(
				levels.some(([deck, gain]) => deck === 0 && isDown2dB(gain)) ? 'play levelled' : 'play'
			);
			return playA.call(a.element);
		};

		playerStore.playQueue([local('one', 'first')]);

		await expect.poll(() => events).toContain('load');
		await new Promise((resolve) => setTimeout(resolve, 20));
		expect(events).toEqual(['load']);
		answer();
		await expect.poll(() => playerStore.playbackState).toBe('playing');
		expect(events).toEqual(['load', 'loudness', 'play levelled']);
	});

	it('clears both decks on a stop mid-fade', async () => {
		crossfade.setSeconds(6);
		await playUntilPreloaded([track('one', 'first', 1), track('two', 'second', 1)]);
		a.at(194);
		await expect.poll(() => b.plays).toEqual([stream('two')]);

		playerStore.stop();

		await expect.poll(() => [a.element.src, b.element.src]).toEqual(['', '']);
		expect(playingDecks([a, b])).toEqual([]);
	});
});
