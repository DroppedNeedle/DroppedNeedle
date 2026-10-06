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
import { api } from '$lib/api/client';
import { SvelteSet } from 'svelte/reactivity';
import { audioFocus } from '$lib/stores/audioFocus.svelte';
import { playbackToast } from '$lib/stores/playbackToast.svelte';
import type { PreviewTrackItem } from '$lib/types';
import { DiscoverV3Api } from '$lib/queries/discover/DiscoverV3Api';

const FOCUS_ID = 'deck-sampler';
const CROSSFADE_S = 0.5;
const TICK_MS = 100;
// Playing this fixed private silent clip once on each pooled element during the
// starting gesture lets later timer-driven source swaps pass browser autoplay
// policy without ever exposing a remote unlock URL.
const SILENT_CLIP =
	'data:audio/wav;base64,UklGRjQAAABXQVZFZm10IBAAAAABAAEAQB8AAIA+AAACABAAZGF0YRAAAAAAAAAAAAAAAAAAAAAAAAAA';
// clips per album when playing a multi-entry station (keeps a lean-back station
// moving); a single-album sample plays everything the backend returns
const STATION_CLIPS_PER_ALBUM = 2;
const VOLUME_KEY = 'droppedneedle_sampler_volume';

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

	type IntervalHandle = ReturnType<typeof setInterval>;
	type TimeoutHandle = ReturnType<typeof setTimeout>;
	type SlotOwner = 'free' | 'unlock' | 'clip';

	interface PoolSlot {
		el: HTMLAudioElement;
		generation: number;
		owner: SlotOwner;
		fadeTimers: Set<IntervalHandle>;
	}

	interface AudioLease {
		slot: PoolSlot;
		element: HTMLAudioElement;
		generation: number;
		session: number;
		owner: SlotOwner;
	}

	let poolA: PoolSlot | null = null;
	let poolB: PoolSlot | null = null;
	let activeLease: AudioLease | null = null;
	let useA = true;
	let ticker: IntervalHandle | null = null;
	const transitionTimers = new SvelteSet<TimeoutHandle>();
	let session = 0;
	let playAttempt = 0;

	function poolSlots(): PoolSlot[] {
		const slots: PoolSlot[] = [];
		if (poolA) slots.push(poolA);
		if (poolB) slots.push(poolB);
		return slots;
	}

	function clearTicker(expected?: IntervalHandle) {
		if (expected !== undefined && ticker !== expected) {
			clearInterval(expected);
			return;
		}
		if (ticker) {
			clearInterval(ticker);
			ticker = null;
		}
	}

	function clearTransitionTimers() {
		for (const timer of transitionTimers) clearTimeout(timer);
		transitionTimers.clear();
	}

	function clearSlotFades(slot: PoolSlot) {
		for (const timer of slot.fadeTimers) clearInterval(timer);
		slot.fadeTimers.clear();
	}

	function clearFade(slot: PoolSlot, timer: IntervalHandle) {
		clearInterval(timer);
		slot.fadeTimers.delete(timer);
	}

	function resetSlot(slot: PoolSlot) {
		clearSlotFades(slot);
		slot.generation++;
		slot.owner = 'free';
		slot.el.pause();
		slot.el.removeAttribute('src');
		slot.el.load();
		slot.el.muted = false;
		slot.el.volume = volume;
	}

	function ensurePool() {
		if (!poolA) {
			const el = new Audio();
			el.preload = 'auto';
			poolA = { el, generation: 0, owner: 'free', fadeTimers: new Set() };
		}
		if (!poolB) {
			const el = new Audio();
			el.preload = 'auto';
			poolB = { el, generation: 0, owner: 'free', fadeTimers: new Set() };
		}
	}

	function isCurrentLease(lease: AudioLease, mySession: number): boolean {
		return (
			mySession === session &&
			lease.session === mySession &&
			lease.slot.el === lease.element &&
			lease.slot.generation === lease.generation &&
			lease.slot.owner === lease.owner
		);
	}

	function currentLease(slot: PoolSlot, mySession: number): AudioLease {
		return {
			slot,
			element: slot.el,
			generation: slot.generation,
			session: mySession,
			owner: slot.owner
		};
	}

	function isCurrentPlayback(lease: AudioLease, mySession: number, attempt: number): boolean {
		return isCurrentLease(lease, mySession) && activeLease === lease && playAttempt === attempt;
	}

	function claimSlot(slot: PoolSlot, mySession: number, owner: SlotOwner): AudioLease {
		resetSlot(slot);
		slot.owner = owner;
		return {
			slot,
			element: slot.el,
			generation: slot.generation,
			session: mySession,
			owner
		};
	}

	/**
	 * This is deliberately the only unlock path. `beginStation` calls it while
	 * handling the user's starting click; `nextEl`/`claimSlot` only create or
	 * replace resources and never attempt a gesture-only play.
	 */
	function unlockPool(mySession: number) {
		ensurePool();
		for (const slot of poolSlots()) {
			const lease = claimSlot(slot, mySession, 'unlock');
			const el = lease.element;
			if (!isCurrentLease(lease, mySession)) continue;
			el.muted = true;
			el.src = SILENT_CLIP;
			try {
				const playPromise = el.play();
				void playPromise.then(
					() => {
						if (!isCurrentLease(lease, mySession)) return;
						el.pause();
						el.muted = false;
					},
					() => {
						if (!isCurrentLease(lease, mySession)) return;
						el.muted = false;
					}
				);
			} catch {
				if (isCurrentLease(lease, mySession)) el.muted = false;
			}
		}
	}

	function clearAllFadeTimers() {
		for (const slot of poolSlots()) clearSlotFades(slot);
	}

	function resetPool() {
		for (const slot of poolSlots()) resetSlot(slot);
	}

	function pausePool(mySession: number) {
		for (const slot of poolSlots()) {
			const lease = currentLease(slot, mySession);
			if (isCurrentLease(lease, mySession)) lease.element.pause();
		}
	}

	function stopInternal() {
		session++;
		playAttempt++;
		clearTicker();
		clearTransitionTimers();
		clearAllFadeTimers();
		activeLease = null;
		resetPool();
		useA = true;
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

	function nextLease(mySession: number): AudioLease {
		ensurePool();
		const slot = useA ? poolA! : poolB!;
		useA = !useA;
		return claimSlot(slot, mySession, 'clip');
	}

	function applyVolumeRamp(
		lease: AudioLease,
		mySession: number,
		fadeIn: boolean,
		crossfading: boolean
	) {
		if (!isCurrentLease(lease, mySession)) return;
		const el = lease.element;
		if (fadeIn) {
			const target = volume;
			const step = (target * TICK_MS) / (CROSSFADE_S * 1000);
			if (el.volume < target) {
				el.volume = Math.min(target, el.volume + step);
			} else if (!crossfading && el.volume > target) {
				el.volume = target;
			}
		} else if (!crossfading && el.volume !== volume) {
			el.volume = volume;
		}
	}

	function fadeOut(lease: AudioLease, mySession: number) {
		if (!isCurrentLease(lease, mySession)) return;
		const slot = lease.slot;
		const el = lease.element;
		clearSlotFades(slot);
		const startVolume = Math.max(0, Math.min(1, el.volume));
		if (startVolume <= 0) {
			el.pause();
			return;
		}
		const stepsTotal = Math.max(1, Math.ceil((CROSSFADE_S * 1000) / TICK_MS));
		let steps = 0;
		const fade = setInterval(() => {
			if (!slot.fadeTimers.has(fade)) {
				clearInterval(fade);
				return;
			}
			if (!isCurrentLease(lease, mySession) || (status !== 'playing' && status !== 'loading')) {
				clearFade(slot, fade);
				return;
			}
			steps++;
			if (steps >= stepsTotal) {
				el.volume = 0;
				clearFade(slot, fade);
				el.pause();
				return;
			}
			el.volume = Math.max(0, startVolume * (1 - steps / stepsTotal));
		}, TICK_MS);
		slot.fadeTimers.add(fade);
	}

	function scheduleTransition(mySession: number, sourceLease: AudioLease, action: () => void) {
		const timer = setTimeout(() => {
			transitionTimers.delete(timer);
			if (mySession !== session || !isCurrentLease(sourceLease, mySession)) return;
			action();
		}, CROSSFADE_S * 1000);
		transitionTimers.add(timer);
	}

	function runTicker(lease: AudioLease, mySession: number, fadeIn: boolean, attempt: number) {
		if (!isCurrentPlayback(lease, mySession, attempt)) return;
		clearTicker();
		let crossfading = false;
		const interval = setInterval(() => {
			if (!isCurrentPlayback(lease, mySession, attempt) || status !== 'playing') {
				clearTicker(interval);
				return;
			}
			const el = lease.element;
			const duration = Number.isFinite(el.duration) && el.duration > 0 ? el.duration : 30;
			progress = Math.min(1, el.currentTime / duration);
			applyVolumeRamp(lease, mySession, fadeIn, crossfading);
			const remaining = duration - el.currentTime;
			if (!crossfading && remaining <= CROSSFADE_S && remaining > 0) {
				crossfading = true;
				void advance(mySession, lease, true);
			}
			if (el.ended) {
				clearTicker(interval);
				if (!crossfading) void advance(mySession, lease);
			}
		}, TICK_MS);
		ticker = interval;
	}

	function isNotAllowedError(error: unknown): boolean {
		if (typeof error !== 'object' || error === null || !('name' in error)) return false;
		return (error as { name?: unknown }).name === 'NotAllowedError';
	}

	function pauseForBlockedPlayback(lease: AudioLease, mySession: number, attempt: number) {
		if (!isCurrentPlayback(lease, mySession, attempt)) return;
		clearTicker();
		clearTransitionTimers();
		clearAllFadeTimers();
		playAttempt++;
		pausePool(mySession);
		status = 'paused';
		playbackToast.show('Tap the preview play button to keep sampling', 'warning');
	}

	async function playTrack(index: number, mySession: number, fadeIn: boolean): Promise<void> {
		const track = tracks[index];
		if (!track || mySession !== session) return;
		trackIndex = index;
		progress = 0;

		const lease = nextLease(mySession);
		const attempt = ++playAttempt;
		activeLease = lease;
		const el = lease.element;
		if (!isCurrentPlayback(lease, mySession, attempt)) return;
		el.src = track.preview_url;
		el.muted = false;
		el.volume = fadeIn ? 0 : volume;
		try {
			await el.play();
		} catch (error) {
			if (!isCurrentPlayback(lease, mySession, attempt)) return;
			if (isNotAllowedError(error)) {
				pauseForBlockedPlayback(lease, mySession, attempt);
			} else {
				void advance(mySession, lease, false, true);
			}
			return;
		}
		if (!isCurrentPlayback(lease, mySession, attempt) || status !== 'playing') return;
		runTicker(lease, mySession, fadeIn, attempt);
	}

	async function advance(
		mySession: number,
		sourceLease?: AudioLease,
		shouldFade = false,
		failedPlayback = false
	): Promise<void> {
		if (mySession !== session) return;
		if (sourceLease && (!isCurrentLease(sourceLease, mySession) || activeLease !== sourceLease)) {
			return;
		}
		const fadeOutLease = shouldFade ? sourceLease : undefined;
		if (fadeOutLease) fadeOut(fadeOutLease, mySession);

		if (trackIndex + 1 < tracks.length) {
			await playTrack(trackIndex + 1, mySession, !!fadeOutLease);
			return;
		}

		if (entryIndex + 1 < station.length) {
			const nextIndex = entryIndex + 1;
			if (fadeOutLease) {
				scheduleTransition(mySession, fadeOutLease, () => {
					void loadEntry(nextIndex, mySession);
				});
			} else {
				await loadEntry(nextIndex, mySession);
			}
			return;
		}

		const currentEntry = station[entryIndex];
		if (failedPlayback && currentEntry) {
			failOrEnd(currentEntry);
		} else if (fadeOutLease) {
			scheduleTransition(mySession, fadeOutLease, stopAll);
		} else {
			stopAll();
		}
	}

	async function fetchEntryTracks(entry: SampleEntry): Promise<{
		tracks: PreviewTrackItem[];
		provider: string | null;
	}> {
		if (entry.kind === 'track') {
			const data = await api.global.v3.GET(DiscoverV3Api.trackPreview(entry.artist, entry.title));
			const provider = data.provider ?? null;
			if (!data.preview_url) return { tracks: [], provider };
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
				provider
			};
		}
		const data = await api.global.v3.GET(
			DiscoverV3Api.albumPreview(entry.artist, entry.title, null)
		);
		const tracks: PreviewTrackItem[] = (data.tracks ?? []).flatMap((track) =>
			track.preview_url
				? [
						{
							title: track.title,
							artist_name: track.artist_name ?? entry.artist,
							preview_url: track.preview_url,
							duration_s: track.duration_s ?? null,
							position: track.position ?? null
						}
					]
				: []
		);
		const limit = station.length > 1 ? STATION_CLIPS_PER_ALBUM : tracks.length;
		return { tracks: tracks.slice(0, limit), provider: data.provider ?? null };
	}

	async function loadEntry(index: number, mySession: number): Promise<void> {
		if (mySession !== session) return;
		clearTicker();
		clearTransitionTimers();
		playAttempt++;
		activeLease = null;
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
		status = 'loading';
		try {
			const result = await fetchEntryTracks(entry);
			if (mySession !== session) return;
			if (result.tracks.length === 0) {
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
			if (index + 1 < station.length) {
				await loadEntry(index + 1, mySession);
			} else {
				failOrEnd(entry);
			}
		}
	}

	/** End the run; the widget hides on stop, so a toast is the only feedback. */
	function failOrEnd(entry: SampleEntry) {
		const wasSingle = station.length === 1;
		stopAll();
		status = 'error';
		playbackToast.show(
			wasSingle
				? entry.kind === 'album'
					? `No preview available for ${entry.title}`
					: `No preview available for that track`
				: 'Preview station ended: no more playable clips',
			'warning'
		);
	}

	function beginStation(title: string, entries: SampleEntry[]): void {
		stopInternal();
		if (entries.length === 0) {
			audioFocus.release(FOCUS_ID);
			return;
		}
		const mySession = session;
		// This call stays in the starting click so both persistent elements inherit
		// the gesture before any fetch/timer can try a real preview.
		unlockPool(mySession);
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
			if (Number.isFinite(v)) volume = Math.min(1, Math.max(0, v));
			if (activeLease && isCurrentLease(activeLease, session)) {
				activeLease.element.volume = volume;
			}
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
			clearTransitionTimers();
			clearAllFadeTimers();
			playAttempt++;
			pausePool(session);
			status = 'paused';
		},

		resume(): void {
			if (status !== 'paused' || !activeLease || !isCurrentLease(activeLease, session)) {
				return;
			}
			const lease = activeLease;
			const mySession = session;
			const attempt = ++playAttempt;
			status = 'playing';
			if (!isCurrentPlayback(lease, mySession, attempt)) return;
			try {
				const playPromise = lease.element.play();
				void playPromise.then(
					() => {
						if (!isCurrentPlayback(lease, mySession, attempt)) return;
						runTicker(lease, mySession, false, attempt);
					},
					(error: unknown) => {
						if (!isCurrentPlayback(lease, mySession, attempt)) return;
						if (isNotAllowedError(error)) {
							pauseForBlockedPlayback(lease, mySession, attempt);
						} else {
							void advance(mySession, lease, false, true);
						}
					}
				);
			} catch (error) {
				if (!isCurrentPlayback(lease, mySession, attempt)) return;
				if (isNotAllowedError(error)) {
					pauseForBlockedPlayback(lease, mySession, attempt);
				} else {
					void advance(mySession, lease, false, true);
				}
			}
		},

		togglePlay(): void {
			if (status === 'playing') this.pause();
			else if (status === 'paused') this.resume();
		},

		/** Skip to the next station entry (no-op on the last one). */
		next(): void {
			if (entryIndex + 1 < station.length) {
				const oldSession = session;
				clearTicker();
				clearTransitionTimers();
				clearAllFadeTimers();
				playAttempt++;
				pausePool(oldSession);
				const nextIndex = entryIndex + 1;
				const mySession = ++session;
				activeLease = null;
				status = 'loading';
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
