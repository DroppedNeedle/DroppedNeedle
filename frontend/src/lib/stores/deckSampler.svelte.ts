/**
 * Global preview player: plays 30s Deezer/iTunes clips with short crossfades -
 * a record-shop listening booth that follows you across the app via the floating
 * PreviewWidget.
 *
 * Deliberately NOT the global music player: previews are cross-origin clips, and
 * the player routes its <audio> through Web Audio (EQ/visualiser), which mutes
 * cross-origin media. Bare Audio() elements here play fine and never disturb the
 * listener's real queue. Obeys the one-sound rule via audioFocus.
 *
 * Two shapes, one pipeline:
 *  - a single album / track (`start` / `startTrack`)
 *  - a station: a queue of album/track entries played back-to-back (`startStation`)
 */
import { API } from '$lib/constants';
import { api } from '$lib/api/client';
import { audioFocus } from '$lib/stores/audioFocus.svelte';
import { playbackToast } from '$lib/stores/playbackToast.svelte';
import type { AlbumPreviewResponse, PreviewTrackItem, TrackPreviewResponse } from '$lib/types';

const FOCUS_ID = 'deck-sampler';
const CROSSFADE_S = 2;
const TICK_MS = 100;
const VOLUME_KEY = 'droppedneedle_sampler_volume';
// clips per album when playing a multi-entry station (keeps a lean-back station
// moving); a single-album sample plays everything the backend returns
const STATION_CLIPS_PER_ALBUM = 2;

function storedVolume(): number {
	try {
		const raw = Number(localStorage.getItem(VOLUME_KEY));
		return Number.isFinite(raw) && raw > 0 && raw <= 1 ? raw : 0.7;
	} catch {
		return 0.7;
	}
}

export type SamplerStatus = 'idle' | 'loading' | 'playing' | 'paused' | 'error';

/** Rendering + navigation context for one thing being sampled. */
export interface SampleEntry {
	/** stable id (release-group mbid for albums, `track:artist|title` for tracks) */
	key: string;
	kind: 'album' | 'track';
	artist: string;
	/** album or track display name */
	title: string;
	albumMbid?: string | null;
	artistMbid?: string | null;
	coverUrl?: string | null;
}

function createDeckSampler() {
	let status = $state<SamplerStatus>('idle');
	let station = $state<SampleEntry[]>([]);
	let entryIndex = $state(0);
	let tracks = $state<PreviewTrackItem[]>([]);
	let trackIndex = $state(0);
	let provider = $state<string | null>(null);
	let progress = $state(0); // 0..1 within the current preview
	// previews can be LOUD: default well under full volume, user-adjustable
	let volume = $state(storedVolume());
	let stationTitle = $state('');

	let audioA: HTMLAudioElement | null = null;
	let audioB: HTMLAudioElement | null = null;
	let activeEl: HTMLAudioElement | null = null;
	let useA = true;
	let ticker: ReturnType<typeof setInterval> | null = null;
	let session = 0;

	function clearTicker() {
		if (ticker) {
			clearInterval(ticker);
			ticker = null;
		}
	}

	function teardownAudio() {
		for (const el of [audioA, audioB]) {
			if (el) {
				el.pause();
				el.src = '';
			}
		}
		audioA = audioB = activeEl = null;
	}

	function stopInternal() {
		session++;
		clearTicker();
		teardownAudio();
		status = 'idle';
		station = [];
		entryIndex = 0;
		tracks = [];
		trackIndex = 0;
		progress = 0;
		provider = null;
		stationTitle = '';
	}

	function stopAll() {
		stopInternal();
		audioFocus.release(FOCUS_ID);
	}

	function nextEl(): HTMLAudioElement {
		const el = new Audio();
		el.preload = 'auto';
		if (useA) {
			audioB = el;
		} else {
			audioA = el;
		}
		return el;
	}

	function applyVolumeRamp(el: HTMLAudioElement, fadeIn: boolean, crossfading: boolean) {
		if (fadeIn && el.volume < volume) {
			el.volume = Math.min(volume, el.volume + (volume * TICK_MS) / (CROSSFADE_S * 1000));
		} else if (!fadeIn && el.volume > volume) {
			// live slider adjustments apply immediately
			el.volume = volume;
		} else if (!crossfading && el.volume < volume) {
			el.volume = volume;
		}
	}

	function runTicker(el: HTMLAudioElement, mySession: number, fadeIn: boolean) {
		clearTicker();
		let crossfading = false;
		ticker = setInterval(() => {
			if (mySession !== session || status === 'paused') {
				clearTicker();
				return;
			}
			const duration = el.duration && isFinite(el.duration) ? el.duration : 30;
			progress = Math.min(1, el.currentTime / duration);
			applyVolumeRamp(el, fadeIn, crossfading);
			const remaining = duration - el.currentTime;
			if (!crossfading && remaining <= CROSSFADE_S && remaining > 0) {
				crossfading = true;
				void advance(mySession, el);
			}
			if (el.ended) {
				clearTicker();
				if (!crossfading) void advance(mySession);
			}
		}, TICK_MS);
	}

	async function playTrack(index: number, mySession: number, fadeIn: boolean): Promise<void> {
		const track = tracks[index];
		if (!track || mySession !== session) return;
		trackIndex = index;
		progress = 0;

		const el = nextEl();
		useA = !useA;
		el.src = track.preview_url;
		el.volume = fadeIn ? 0 : volume;
		activeEl = el;
		try {
			await el.play();
		} catch {
			// unplayable preview (expired URL, codec): skip forward
			if (mySession === session) void advance(mySession);
			return;
		}
		runTicker(el, mySession, fadeIn);
	}

	function fadeOut(el: HTMLAudioElement) {
		const fade = setInterval(() => {
			el.volume = Math.max(0, el.volume - (volume * TICK_MS) / (CROSSFADE_S * 1000));
			if (el.volume <= 0) {
				clearInterval(fade);
				el.pause();
			}
		}, TICK_MS);
	}

	async function advance(mySession: number, fadeOutEl?: HTMLAudioElement): Promise<void> {
		if (mySession !== session) return;
		if (fadeOutEl) fadeOut(fadeOutEl);

		// more clips in this entry -> crossfade within the album
		if (trackIndex + 1 < tracks.length) {
			await playTrack(trackIndex + 1, mySession, !!fadeOutEl);
			return;
		}

		// entry exhausted -> next entry in the station (hard cut between albums)
		if (entryIndex + 1 < station.length) {
			if (fadeOutEl) {
				// let the last clip fade out, then load the next entry
				setTimeout(() => {
					if (mySession === session) void loadEntry(entryIndex + 1, mySession);
				}, CROSSFADE_S * 1000);
			} else {
				await loadEntry(entryIndex + 1, mySession);
			}
			return;
		}

		// natural end of the whole station
		if (fadeOutEl) {
			setTimeout(() => {
				if (mySession === session) stopAll();
			}, CROSSFADE_S * 1000);
		} else {
			stopAll();
		}
	}

	async function fetchEntryTracks(entry: SampleEntry): Promise<{
		tracks: PreviewTrackItem[];
		provider: string | null;
	}> {
		if (entry.kind === 'track') {
			const data = await api.global.get<TrackPreviewResponse>(
				API.discoverTrackPreview(entry.artist, entry.title)
			);
			if (!data.preview_url) return { tracks: [], provider: data.provider };
			return {
				tracks: [
					{
						title: data.title ?? entry.title,
						artist_name: entry.artist,
						preview_url: data.preview_url,
						duration_s: data.duration_s ?? 30,
						position: 1
					}
				],
				provider: data.provider
			};
		}
		const data = await api.global.get<AlbumPreviewResponse>(
			API.discoverAlbumPreview(entry.artist, entry.title)
		);
		const limit = station.length > 1 ? STATION_CLIPS_PER_ALBUM : data.tracks.length;
		return { tracks: data.tracks.slice(0, limit), provider: data.provider };
	}

	async function loadEntry(index: number, mySession: number): Promise<void> {
		if (mySession !== session) return;
		clearTicker(); // a stale ticker from the previous entry must not double-fire
		const entry = station[index];
		if (!entry) {
			stopAll();
			return;
		}
		entryIndex = index;
		tracks = [];
		trackIndex = 0;
		progress = 0;
		provider = null;
		status = status === 'idle' ? 'loading' : status;
		try {
			const result = await fetchEntryTracks(entry);
			if (mySession !== session) return;
			if (result.tracks.length === 0) {
				// nothing playable for this entry -> try the next one
				if (index + 1 < station.length) {
					await loadEntry(index + 1, mySession);
				} else {
					failOrEnd(entry);
				}
				return;
			}
			tracks = result.tracks;
			provider = result.provider;
			status = 'playing';
			await playTrack(0, mySession, false);
		} catch {
			if (mySession !== session) return;
			// transient fetch failure: skip to the next entry, or give up
			if (index + 1 < station.length) {
				await loadEntry(index + 1, mySession);
			} else {
				failOrEnd(entry);
			}
		}
	}

	/** End the run; on a single-item preview, tell the user it was unavailable
	 * (the widget hides on 'error', so a toast is the only feedback). */
	function failOrEnd(entry: SampleEntry) {
		const wasSingle = station.length === 1;
		stopAll();
		if (wasSingle) {
			status = 'error';
			playbackToast.show(
				entry.kind === 'album'
					? `No preview available for ${entry.title}`
					: `No preview available for that track`,
				'warning'
			);
		}
	}

	function beginStation(title: string, entries: SampleEntry[]): void {
		stopInternal();
		if (entries.length === 0) return;
		const mySession = ++session;
		audioFocus.claim(FOCUS_ID, stopAll);
		status = 'loading';
		station = entries;
		stationTitle = title;
		void loadEntry(0, mySession);
	}

	const store = {
		get status() {
			return status;
		},
		get tracks() {
			return tracks;
		},
		get trackIndex() {
			return trackIndex;
		},
		get currentTrack() {
			return tracks[trackIndex] ?? null;
		},
		get currentEntry(): SampleEntry | null {
			return station[entryIndex] ?? null;
		},
		get provider() {
			return provider;
		},
		get progress() {
			return progress;
		},
		get activeKey() {
			return station[entryIndex]?.key ?? '';
		},
		get volume() {
			return volume;
		},
		get isStation() {
			return station.length > 1;
		},
		get stationTitle() {
			return stationTitle;
		},
		get stationPosition() {
			return { index: entryIndex, total: station.length };
		},
		get hasNext() {
			return entryIndex + 1 < station.length;
		},

		setVolume(v: number): void {
			volume = Math.min(1, Math.max(0, v));
			if (activeEl) activeEl.volume = volume;
			try {
				localStorage.setItem(VOLUME_KEY, String(volume));
			} catch {
				/* ignore */
			}
		},

		/** Single album: play its clips back-to-back with crossfades. */
		start(
			key: string,
			artist: string,
			album: string,
			ctx: Partial<Omit<SampleEntry, 'key' | 'kind' | 'artist' | 'title'>> = {}
		): void {
			if (currentKey() === key && (status === 'playing' || status === 'loading')) return;
			beginStation(album, [
				{ key, kind: 'album', artist, title: album, albumMbid: ctx.albumMbid ?? key, ...ctx }
			]);
		},

		/** Single track: one 30s clip. */
		startTrack(
			key: string,
			artist: string,
			track: string,
			ctx: Partial<Omit<SampleEntry, 'key' | 'kind' | 'artist' | 'title'>> = {}
		): void {
			if (currentKey() === key && (status === 'playing' || status === 'loading')) return;
			beginStation(track, [{ key, kind: 'track', artist, title: track, ...ctx }]);
		},

		/** A queue of entries played back-to-back (Lounge "Play all", genre previews). */
		startStation(title: string, entries: SampleEntry[]): void {
			beginStation(title, entries);
		},

		pause(): void {
			if (status !== 'playing') return;
			clearTicker();
			for (const el of [audioA, audioB]) el?.pause();
			status = 'paused';
		},

		resume(): void {
			if (status !== 'paused' || !activeEl) return;
			status = 'playing';
			const el = activeEl;
			const mySession = session;
			el.play().then(
				() => {
					if (mySession === session && status === 'playing') runTicker(el, mySession, false);
				},
				() => {
					// resume rejected (e.g. the preview URL expired while paused): skip
					// forward instead of stalling a ticker on a dead element
					if (mySession === session && status === 'playing') void advance(mySession);
				}
			);
		},

		togglePlay(): void {
			if (status === 'playing') this.pause();
			else if (status === 'paused') this.resume();
		},

		/** Skip to the next station entry (no-op on the last one). */
		next(): void {
			if (entryIndex + 1 < station.length) {
				clearTicker();
				for (const el of [audioA, audioB]) el?.pause(); // cut the current clip cleanly
				const nextIndex = entryIndex + 1;
				// bump the session so any in-flight loadEntry/advance chain (a rapid
				// second skip, or a pending crossfade setTimeout) is invalidated and
				// can't start a second <audio> alongside this one (one-sound rule)
				const mySession = ++session;
				void loadEntry(nextIndex, mySession);
			}
		},

		stop(): void {
			stopAll();
		}
	};

	function currentKey(): string {
		return station[entryIndex]?.key ?? '';
	}

	return store;
}

export const deckSampler = createDeckSampler();
