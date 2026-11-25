/**
 * Global preview player: plays 30s Deezer/iTunes clips with short crossfades -
 * a record-shop listening booth that follows you across the app via the floating
 * PreviewWidget.
 *
 * Deliberately NOT the global music player: previews are cross-origin clips, and
 * the player routes its <audio> through Web Audio (EQ/visualiser), which mutes
 * cross-origin media. Bare Audio() elements here play fine and never disturb the
 * listener's real queue. Obeys the one-sound rule via audioFocus. A clip is a
 * taste, never a play: nothing here reaches the player, its scrobbles or its
 * playback events.
 *
 * Two shapes, one pipeline:
 *  - a single album / track (`start` / `startTrack`)
 *  - a station: a queue of album/track entries played back-to-back (`startStation`)
 *
 * A station's albums are auditioned (opener, best known, deep cut). While one
 * plays, the next album's clips are fetched, and once its last clip plays the
 * next album's first clip is loaded into the idle element. Clips move on from
 * the element's own `timeupdate` and `ended` events as well as a ticker, so a
 * phone with its screen off keeps going. The lock screen reads `lockScreen`
 * through the shell's one media session.
 */
import { SvelteMap, SvelteSet } from 'svelte/reactivity';
import { audioFocus } from '$lib/features/player/state/audioFocus.svelte';
import type { NowPlaying, PlaybackState } from '$lib/features/player/playback/types';
import { toastStore } from '$lib/stores/toast';
import { outsideLink } from '$lib/ui/outsideLink';
import type {
	AlbumPreviewMode,
	PreviewSource,
	PreviewTrackItem
} from '$lib/features/discover/queries/types';
import { fetchAlbumPreview, fetchTrackPreview } from '$lib/features/discover/queries/previews';
import { pickAnswers } from './pickAnswers.svelte';

const FOCUS_ID = 'deck-sampler';
const CROSSFADE_S = 0.5;
const TICK_MS = 100;
const CLIP_S = 30;
// Playing this fixed private silent clip once on each pooled element during the
// starting gesture lets later timer-driven source swaps pass browser autoplay
// policy without ever exposing a remote unlock URL.
const SILENT_CLIP =
	'data:audio/wav;base64,UklGRjQAAABXQVZFZm10IBAAAAABAAEAQB8AAIA+AAACABAAZGF0YRAAAAAAAAAAAAAAAAAAAAAAAAAA';
// clips per album in a station of first tracks (radio and genre previews),
// which keeps a lean-back station moving
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

/** One clip the sampler plays. */
export type SampleClip = Omit<PreviewTrackItem, 'source'> & {
	/** The service it comes from; a track preview may not say. */
	source?: PreviewSource | null;
};

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
	/**
	 * Which clips an album plays: `first` its first tracks, `audition` three
	 * that show it (`first` by default; a station of first tracks plays two
	 * an album). Pressing play again on a stopped audition moves on one clip.
	 */
	mode?: AlbumPreviewMode;
	/** Called once a run, when the entry's first clip starts. */
	onFirstClip?: () => void;
	/**
	 * A Discover pick, and where it was made: the widget offers Keep and
	 * Later while it plays, through the Discover page when it is mounted.
	 */
	pick?: 'weekly_set' | 'dig_further';
}

/** What the lock screen reads; the shell's media session takes it in place of the player's. */
export interface SamplerLockScreen {
	nowPlaying: NowPlaying;
	playbackState: PlaybackState;
	/** Seconds into the clip. */
	progress: number;
	/** The clip's length in seconds. */
	duration: number;
}

const SOURCES: Record<string, PreviewSource> = { deezer: 'deezer', itunes: 'itunes' };

function createDeckSampler() {
	let status = $state<SamplerStatus>('idle');
	let station = $state<SampleEntry[]>([]);
	let entryIndex = $state(0);
	let tracks = $state<SampleClip[]>([]);
	let trackIndex = $state(0);
	let provider = $state<string | null>(null);
	let progress = $state(0); // 0..1 within the current preview
	let clipLength = $state(CLIP_S);
	// previews can be LOUD: default well under full volume, user-adjustable
	let volume = $state(storedVolume());
	let stationTitle = $state('');
	let stationTag = $state<string | null>(null);

	type IntervalHandle = ReturnType<typeof setInterval>;
	type TimeoutHandle = ReturnType<typeof setTimeout>;
	type SlotOwner = 'free' | 'unlock' | 'clip' | 'preload';

	interface PoolSlot {
		el: HTMLAudioElement;
		generation: number;
		owner: SlotOwner;
		fadeTimers: Set<IntervalHandle>;
		/** The element's own event listeners for the clip it plays. */
		events: AbortController | null;
		/** The clip loaded ahead into this idle element, if any. */
		preloaded: string | null;
	}

	interface AudioLease {
		slot: PoolSlot;
		element: HTMLAudioElement;
		generation: number;
		session: number;
		owner: SlotOwner;
	}

	/** One clip playing: its lease and how far it has got. */
	interface ClipRun {
		lease: AudioLease;
		session: number;
		attempt: number;
		fadeIn: boolean;
		crossfading: boolean;
	}

	let poolA: PoolSlot | null = null;
	let poolB: PoolSlot | null = null;
	let activeLease: AudioLease | null = null;
	let useA = true;
	let ticker: IntervalHandle | null = null;
	const transitionTimers = new SvelteSet<TimeoutHandle>();
	let pendingTransition: { lease: AudioLease; session: number; run: () => void } | null = null;
	let session = 0;
	let playAttempt = 0;
	// Per run: the entries whose first clip started, and whether any did.
	const firstClips = new SvelteSet<number>();
	let heardAny = false;
	// The next station entry's clips, fetched while this one plays.
	let ahead: { index: number; promise: Promise<FetchedClips>; clips: SampleClip[] | null } | null =
		null;
	// Where a stopped audition picks up when its play is pressed again.
	const resumeAt = new SvelteMap<string, number>();

	type FetchedClips = { tracks: SampleClip[]; provider: string | null };

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
		pendingTransition = null;
	}

	function clearSlotFades(slot: PoolSlot) {
		for (const timer of slot.fadeTimers) clearInterval(timer);
		slot.fadeTimers.clear();
	}

	function clearFade(slot: PoolSlot, timer: IntervalHandle) {
		clearInterval(timer);
		slot.fadeTimers.delete(timer);
	}

	function dropEvents(slot: PoolSlot) {
		slot.events?.abort();
		slot.events = null;
	}

	function resetSlot(slot: PoolSlot) {
		clearSlotFades(slot);
		dropEvents(slot);
		slot.generation++;
		slot.owner = 'free';
		slot.preloaded = null;
		slot.el.pause();
		slot.el.removeAttribute('src');
		slot.el.load();
		slot.el.muted = false;
		slot.el.volume = volume;
	}

	function newSlot(): PoolSlot {
		const el = new Audio();
		el.preload = 'auto';
		return {
			el,
			generation: 0,
			owner: 'free',
			fadeTimers: new Set(),
			events: null,
			preloaded: null
		};
	}

	function ensurePool() {
		if (!poolA) poolA = newSlot();
		if (!poolB) poolB = newSlot();
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

	/** Takes a slot for `owner`; a clip loaded ahead into it as `keep` stays loaded. */
	function claimSlot(
		slot: PoolSlot,
		mySession: number,
		owner: SlotOwner,
		keep?: string
	): AudioLease {
		if (keep && slot.owner === 'preload' && slot.preloaded === keep) {
			clearSlotFades(slot);
			dropEvents(slot);
			slot.generation++;
			slot.el.muted = false;
			slot.el.volume = volume;
		} else {
			resetSlot(slot);
		}
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
		const entry = station[entryIndex];
		if (
			entry?.mode === 'audition' &&
			station.length === 1 &&
			tracks.length > 0 &&
			(status === 'playing' || status === 'paused')
		) {
			resumeAt.set(entry.key, (trackIndex + 1) % tracks.length);
		}
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
		clipLength = CLIP_S;
		provider = null;
		stationTitle = '';
		stationTag = null;
		firstClips.clear();
		heardAny = false;
		ahead = null;
	}

	function stopAll() {
		stopInternal();
		audioFocus.release(FOCUS_ID);
	}

	function nextLease(mySession: number, keep?: string): AudioLease {
		ensurePool();
		const slot = useA ? poolA! : poolB!;
		useA = !useA;
		return claimSlot(slot, mySession, 'clip', keep);
	}

	/** A fade-in follows the clip's own clock, so it holds even when timers are slowed. */
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
			const ramped = Math.min(target, (target * Math.max(0, el.currentTime)) / CROSSFADE_S);
			if (el.volume < ramped) {
				el.volume = ramped;
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

	/** Runs `action` once the fade is through, or when the fading clip ends, whichever is first. */
	function scheduleTransition(mySession: number, sourceLease: AudioLease, action: () => void) {
		const pending = {
			lease: sourceLease,
			session: mySession,
			run: () => {
				if (pendingTransition !== pending) return;
				pendingTransition = null;
				clearTimeout(timer);
				transitionTimers.delete(timer);
				if (mySession !== session || !isCurrentLease(sourceLease, mySession)) return;
				action();
			}
		};
		const timer = setTimeout(pending.run, CROSSFADE_S * 1000);
		pendingTransition = pending;
		transitionTimers.add(timer);
	}

	function flushTransition(lease: AudioLease) {
		if (pendingTransition?.lease === lease) pendingTransition.run();
	}

	/** One look at the playing clip: progress, the fade, the next album's clip, moving on. */
	function step(run: ClipRun): boolean {
		const { lease, session: mySession, attempt } = run;
		if (!isCurrentPlayback(lease, mySession, attempt) || status !== 'playing') return false;
		const el = lease.element;
		const known = Number.isFinite(el.duration) && el.duration > 0;
		const duration = known ? el.duration : (tracks[trackIndex]?.duration_s ?? CLIP_S);
		clipLength = duration;
		progress = Math.min(1, el.currentTime / duration);
		applyVolumeRamp(lease, mySession, run.fadeIn, run.crossfading);
		preloadAhead(mySession);
		const remaining = duration - el.currentTime;
		if (!run.crossfading && remaining <= CROSSFADE_S && remaining > 0) {
			run.crossfading = true;
			void advance(mySession, lease, true);
		}
		if (el.ended && !run.crossfading) {
			run.crossfading = true;
			void advance(mySession, lease);
		}
		return true;
	}

	function runTicker(lease: AudioLease, mySession: number, fadeIn: boolean, attempt: number) {
		if (!isCurrentPlayback(lease, mySession, attempt)) return;
		clearTicker();
		const run: ClipRun = { lease, session: mySession, attempt, fadeIn, crossfading: false };
		const interval = setInterval(() => {
			if (!step(run) || lease.element.ended) clearTicker(interval);
		}, TICK_MS);
		ticker = interval;
		// A hidden page slows its timers; the element's own events keep the clip moving.
		dropEvents(lease.slot);
		const events = new AbortController();
		lease.slot.events = events;
		const el = lease.element;
		el.addEventListener('timeupdate', () => step(run), { signal: events.signal });
		el.addEventListener(
			'ended',
			() => {
				step(run);
				flushTransition(lease);
			},
			{ signal: events.signal }
		);
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
		toastStore.notify('Tap the preview play button to keep sampling', 'warning');
	}

	/** A clip is sounding: the entry's first clip is reported, and the next album fetched. */
	function clipStarted(mySession: number) {
		heardAny = true;
		if (!firstClips.has(entryIndex)) {
			firstClips.add(entryIndex);
			try {
				station[entryIndex]?.onFirstClip?.();
			} catch {
				// a caller's report never stops the sound
			}
		}
		if (station.length > 1) fetchAhead(mySession);
	}

	async function playTrack(index: number, mySession: number, fadeIn: boolean): Promise<void> {
		const track = tracks[index];
		if (!track || mySession !== session) return;
		trackIndex = index;
		progress = 0;

		const lease = nextLease(mySession, track.preview_url);
		const attempt = ++playAttempt;
		activeLease = lease;
		const el = lease.element;
		if (!isCurrentPlayback(lease, mySession, attempt)) return;
		const loaded = lease.slot.preloaded === track.preview_url;
		lease.slot.preloaded = null;
		if (!loaded) el.src = track.preview_url;
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
		clipStarted(mySession);
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

	async function fetchEntryTracks(entry: SampleEntry, alone: boolean): Promise<FetchedClips> {
		if (entry.kind === 'track') {
			const data = await fetchTrackPreview(entry.artist, entry.title);
			const provider = data.provider ?? null;
			const previewUrl = outsideLink(data.preview_url);
			if (!previewUrl) return { tracks: [], provider };
			return {
				tracks: [
					{
						title: data.title ?? entry.title,
						artist_name: entry.artist,
						preview_url: previewUrl,
						duration_s: data.duration_s ?? CLIP_S,
						position: 1,
						source: provider ? (SOURCES[provider] ?? null) : null
					}
				],
				provider
			};
		}
		const mode = entry.mode ?? 'first';
		const data = await fetchAlbumPreview(entry.artist, entry.title, mode);
		const tracks: SampleClip[] = (data.tracks ?? []).flatMap((track) => {
			const previewUrl = outsideLink(track.preview_url);
			return previewUrl
				? [
						{
							title: track.title,
							artist_name: track.artist_name ?? entry.artist,
							preview_url: previewUrl,
							duration_s: track.duration_s ?? null,
							position: track.position ?? null,
							role: track.role ?? null,
							source: track.source
						}
					]
				: [];
		});
		const limit = mode === 'first' && !alone ? STATION_CLIPS_PER_ALBUM : tracks.length;
		return { tracks: tracks.slice(0, limit), provider: data.provider ?? null };
	}

	/** Fetches the station's next entry while this one plays: one ahead, no further. */
	function fetchAhead(mySession: number) {
		const index = entryIndex + 1;
		const entry = station[index];
		if (!entry || ahead?.index === index) return;
		const promise = fetchEntryTracks(entry, false);
		const mine = { index, promise, clips: null as SampleClip[] | null };
		ahead = mine;
		void promise.then(
			(result) => {
				if (ahead !== mine) return;
				mine.clips = result.tracks;
				preloadAhead(mySession);
			},
			() => {
				// loadEntry meets the same failure when it gets there and passes the entry over
			}
		);
	}

	/** Once the album's last clip plays, the next album's first clip loads into the idle element. */
	function preloadAhead(mySession: number) {
		if (mySession !== session || status !== 'playing') return;
		const url = ahead?.index === entryIndex + 1 ? ahead.clips?.[0]?.preview_url : undefined;
		if (!url || trackIndex !== tracks.length - 1) return;
		const slot = useA ? poolA : poolB;
		if (!slot || slot.preloaded === url || slot.fadeTimers.size > 0) return;
		if (activeLease?.slot === slot) return;
		resetSlot(slot);
		slot.owner = 'preload';
		slot.preloaded = url;
		slot.el.src = url;
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
		// a pick answered since the station started is passed over
		if (station.length > 1 && entry.pick && pickAnswers.current?.open(entry.key) === false) {
			if (index + 1 < station.length) await loadEntry(index + 1, mySession);
			else stopAll();
			return;
		}
		entryIndex = index;
		tracks = [];
		trackIndex = 0;
		progress = 0;
		provider = null;
		status = 'loading';
		try {
			const pending =
				ahead?.index === index ? ahead.promise : fetchEntryTracks(entry, station.length === 1);
			const result = await pending;
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
			const from =
				entry.mode === 'audition' && station.length === 1
					? (resumeAt.get(entry.key) ?? 0) % result.tracks.length
					: 0;
			await playTrack(from, mySession, false);
		} catch {
			if (mySession !== session) return;
			if (index + 1 < station.length) {
				await loadEntry(index + 1, mySession);
			} else {
				failOrEnd(entry);
			}
		}
	}

	/**
	 * End the run; the widget hides on stop, so a toast is the only feedback. A
	 * station that played something just ends: an album without clips is passed over.
	 */
	function failOrEnd(entry: SampleEntry) {
		const wasSingle = station.length === 1;
		const played = heardAny;
		stopAll();
		if (!wasSingle && played) return;
		status = 'error';
		toastStore.notify(
			wasSingle
				? entry.kind === 'album'
					? `No preview available for ${entry.title}`
					: `No preview available for that track`
				: 'Preview station ended: no more playable clips',
			'warning'
		);
	}

	function beginStation(title: string, entries: SampleEntry[], tag: string | null): void {
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
		stationTag = tag;
		void loadEntry(0, mySession);
	}

	/** Plays clip `index` of the loaded album now. */
	function skipTo(index: number): void {
		if (status !== 'playing' && status !== 'paused') return;
		if (!tracks[index]) return;
		clearTicker();
		clearTransitionTimers();
		clearAllFadeTimers();
		playAttempt++;
		pausePool(session);
		status = 'playing';
		void playTrack(index, session, false);
	}

	// What the lock screen shows: one object a clip, so progress does not remake it.
	const lockNow = $derived.by((): NowPlaying | null => {
		const entry = station[entryIndex];
		if (!entry || (status !== 'loading' && status !== 'playing' && status !== 'paused')) {
			return null;
		}
		return {
			albumId: entry.albumMbid ?? '',
			albumName: entry.kind === 'album' ? entry.title : '',
			artistName: entry.artist,
			coverUrl: entry.coverUrl ?? null,
			sourceType: 'local',
			trackName: tracks[trackIndex]?.title ?? entry.title,
			isPreview: true
		};
	});

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
		get currentTrack(): SampleClip | null {
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
		/** What started the station, as its caller named it ("week" for Play the week). */
		get stationTag() {
			return stationTag;
		},
		get stationPosition() {
			return { index: entryIndex, total: station.length };
		},
		get hasNext() {
			return entryIndex + 1 < station.length;
		},
		/** The sampler as the lock screen shows it; null while nothing plays. */
		get lockScreen(): SamplerLockScreen | null {
			if (!lockNow) return null;
			return {
				nowPlaying: lockNow,
				playbackState:
					status === 'paused' ? 'paused' : status === 'loading' ? 'loading' : 'playing',
				progress: progress * clipLength,
				duration: tracks[trackIndex] ? clipLength : 0
			};
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
			beginStation(
				album,
				[{ key, kind: 'album', artist, title: album, albumMbid: ctx.albumMbid ?? key, ...ctx }],
				null
			);
		},

		/** Single track: one 30s clip. */
		startTrack(
			key: string,
			artist: string,
			track: string,
			ctx: Partial<Omit<SampleEntry, 'key' | 'kind' | 'artist' | 'title'>> = {}
		): void {
			if (currentKey() === key && (status === 'playing' || status === 'loading')) return;
			beginStation(track, [{ key, kind: 'track', artist, title: track, ...ctx }], null);
		},

		/** A queue of entries played back-to-back (Play the week, radio previews, genre previews). */
		startStation(title: string, entries: SampleEntry[], tag: string | null = null): void {
			beginStation(title, entries, tag);
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
						clipStarted(mySession);
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

		/** The next clip; past an album's last, the station's next album, else the end. */
		nextClip(): void {
			if (status !== 'playing' && status !== 'paused') return;
			if (trackIndex + 1 < tracks.length) skipTo(trackIndex + 1);
			else if (entryIndex + 1 < station.length) this.next();
			else stopAll();
		},

		/** The clip before, or this one from its start. */
		previousClip(): void {
			skipTo(Math.max(0, trackIndex - 1));
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
