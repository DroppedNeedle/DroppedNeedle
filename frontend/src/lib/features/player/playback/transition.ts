/**
 * Every move from one track to the next in the web player. The next track
 * (B) is loaded ahead on the idle deck; near the end of the current one (A)
 * B starts a moment early and the two join, or blend over the listener's
 * crossfade, on the audio engine's gains. With smart fades the blend runs
 * where A's sound fades out, by the sound edges the server measured, which
 * ride on the records here; songs that follow each other on an album never
 * use them. A then fades out as a tail this module owns until it ends.
 * A's own end is the fallback: B starts there when it was not ready, a
 * length could not be trusted, or there is no engine. YouTube tracks never
 * get here: the player loads them as before.
 */
import { tryGetAudioEngine } from './audioElement';
import { followsOnAlbum } from './albumOrder';
import { earliestStart, fadeWindow, type Move, type SoundEdges } from './fadeWindow';
import { NativeAudioSource } from './NativeAudioSource';
import type { TranscodeFormat } from './transcodeTarget';
import type { PlaybackSource, QueueItem } from './types';
import { crossfade } from '../state/crossfade.svelte';

// B starts this long before A ends, so a start timer up to about 60 ms late
// still comes before A's end: a short overlap rather than a gap.
const JOIN_LEAD_S = 0.08;
// From B's first sound, A fades out and B in, in a straight line, over what is
// left of A (so none of A is cut), held to these bounds.
const JOIN_MIN_S = 0.005;
const JOIN_MAX_S = 0.1;
// A sound that ends this close to the song's end ends with the song.
const END_SLACK_S = 0.5;
// The start's timer is set only this close to it, from the latest position.
const ARM_WINDOW_S = 1.5;
// How fast a listener's skip, pause or seek takes the outgoing song away.
const CUT_S = 0.05;
// How fast an older tail goes when the next load needs its deck.
const QUICK_CUT_S = 0.005;
// A tail is ended this long after its fade should have finished.
const TAIL_GRACE_MS = 250;
const QUICK_GRACE_MS = 15;
// A ready B that has not started within this plays again on A's deck.
const JOIN_LIMIT_MS = 2000;
// What a play() the browser refused reports (NativeAudioSource).
const AUTOPLAY_BLOCKED = 'AUTOPLAY_BLOCKED';
const DECKS = 2;

/** The track playing now, as the player loaded it. */
export interface Playing {
	source: PlaybackSource;
	item: QueueItem | null;
	/** Plays as a live transcode, whose element has no length of its own. */
	transcoded: boolean;
	/** Where its sound fades out and ends, once known. */
	edges?: SoundEdges | null;
}

/** The next track, loaded ahead on the idle deck and not playing. */
export interface Preload {
	item: QueueItem;
	source: NativeAudioSource;
	format: TranscodeFormat | null;
	seekable: boolean;
	/** Where its sound fades out and ends, when its loudness read gave them. */
	edges?: SoundEdges | null;
}

export interface TransitionHooks {
	/**
	 * Makes the preloaded track the current one, just before it plays.
	 * False when it no longer can be (it left the queue).
	 */
	handover(next: Preload): boolean;
	/**
	 * The next track did not start at its turn (the browser refused it, or
	 * it did not begin in time): it is to be loaded again, which is not a failure.
	 */
	refused(next: Preload): void;
}

interface Tail {
	source: NativeAudioSource;
	timer: ReturnType<typeof setTimeout> | null;
}

interface Join {
	source: NativeAudioSource;
	/** The crossfade's length; 0 for a join. Null once B started on A's end. */
	fade: number | null;
	/** The song B joins from, fading out. */
	from: Tail | null;
	limit: ReturnType<typeof setTimeout> | null;
}

/**
 * Whether A's own length agrees with the catalog's closely enough to time a
 * join on. A whole-second catalog length (a media server, a playlist row)
 * allows a second; a library file's scanned length, a tenth.
 */
function lengthsAgree(item: QueueItem, elementLength: number): boolean {
	const catalog = item.duration ?? 0;
	if (catalog <= 0 || elementLength <= 0) return false;
	const tolerance = Number.isInteger(catalog) || item.sourceType !== 'local' ? 1 : 0.1;
	return Math.abs(elementLength - catalog) <= tolerance;
}

/** A join lasts what is left of A, by its element; the longest join while that is unknown. */
function joinSeconds(a: NativeAudioSource): number {
	const length = a.elementDuration();
	if (length <= 0) return JOIN_MAX_S;
	return Math.min(JOIN_MAX_S, Math.max(JOIN_MIN_S, length - a.getCurrentTime()));
}

function runningEngine() {
	const engine = tryGetAudioEngine();
	return engine?.running ? engine : null;
}

function isPlayingNative(source: PlaybackSource | undefined): source is NativeAudioSource {
	return source instanceof NativeAudioSource && source.isAudible();
}

export function createTransition(hooks: TransitionHooks) {
	let current: Playing | null = null;
	let next: Preload | null = null;
	let tails: Tail[] = [];
	let join: Join | null = null;
	let armTimer: ReturnType<typeof setTimeout> | null = null;
	// Loads waiting for fading songs to let go of a deck.
	let deckWaiters: { enough: () => boolean; resolve: () => void }[] = [];

	function clearArm(): void {
		if (armTimer) clearTimeout(armTimer);
		armTimer = null;
	}

	function clearJoin(): void {
		if (join?.limit) clearTimeout(join.limit);
		join = null;
	}

	function endTail(tail: Tail): void {
		if (!tails.includes(tail)) return;
		if (tail.timer) clearTimeout(tail.timer);
		tails = tails.filter((t) => t !== tail);
		tail.source.destroy();
		const ready = deckWaiters.filter((waiter) => waiter.enough());
		deckWaiters = deckWaiters.filter((waiter) => !ready.includes(waiter));
		for (const waiter of ready) waiter.resolve();
	}

	function endTailIn(tail: Tail, ms: number): void {
		if (tail.timer) clearTimeout(tail.timer);
		tail.timer = setTimeout(() => endTail(tail), ms);
	}

	/** `source` becomes a tail, ended when it ends or fails, or by its timer. */
	function makeTail(source: NativeAudioSource): Tail {
		const tail: Tail = { source, timer: null };
		tails = [...tails, tail];
		source.onStateChange((state) => {
			if (state === 'ended' || state === 'error') endTail(tail);
		});
		return tail;
	}

	/** Fades a tail out over `seconds` and ends it, or at once while it can't be heard. */
	function fadeOut(tail: Tail, seconds: number, graceMs: number): void {
		const engine = runningEngine();
		if (!engine || !tail.source.isAudible()) return endTail(tail);
		engine.fade(tail.source.lease.deck, 0, seconds, 'linear');
		endTailIn(tail, seconds * 1000 + graceMs);
	}

	function dropPreload(): void {
		if (!next) return;
		next.source.destroy();
		next = null;
	}

	/** A's sound edges, while smart fades are on. */
	function edgesFor(a: Playing): SoundEdges | null {
		return crossfade.smart ? (a.edges ?? null) : null;
	}

	/**
	 * How the move to B goes, from where A is: when B starts and the
	 * crossfade's length (0 for a join started early), or null for B to
	 * start on A's end. A crossfade runs on A's own length (a transcode's is
	 * the catalog's), from A's sound edges unless B follows A on its album;
	 * a join only when that length agrees with the catalog's, since it is
	 * timed to it.
	 */
	function plan(a: Playing, b: Preload, position: number, length: number): Move | null {
		if (!a.item || !(a.source instanceof NativeAudioSource)) return null;
		const { seconds, insideAlbums } = crossfade.settings;
		const onAlbum = followsOnAlbum(a.item, b.item);
		if (seconds > 0 && (insideAlbums || !onAlbum)) {
			const shorter = Math.min(length, b.item.duration || b.source.getDuration() || length);
			const edges = onAlbum ? null : edgesFor(a);
			const move = fadeWindow({ position, length, setting: seconds, shorter, edges });
			// A hard stop at the song's own end is a join, with the join's guards.
			if (move && (move.fade > 0 || move.at < length - END_SLACK_S)) return move;
		}
		// A pipe's end, or an MP3 pipe's start (no gapless header), is not known to the millisecond.
		if (a.transcoded || b.format === 'mp3') return null;
		return lengthsAgree(a.item, a.source.elementDuration())
			? { at: length - JOIN_LEAD_S, fade: 0 }
			: null;
	}

	/** Moves to B: early (a join or a crossfade), or on A's end. False when B could not take over. */
	function start(fade: number | null): boolean {
		clearArm();
		const a = current;
		const b = next;
		if (!a || !b) return false;
		// Early only while A plays, and only once B is buffered enough; else B starts on A's end.
		if (fade !== null && (!isPlayingNative(a.source) || !b.source.isReady())) return false;
		next = null;
		if (!hooks.handover(b)) {
			b.source.destroy();
			return false;
		}
		current = {
			source: b.source,
			item: b.item,
			transcoded: b.format !== null,
			edges: b.edges ?? null
		};
		clearJoin();
		const entry: Join = { source: b.source, fade, from: null, limit: null };
		if (fade !== null && a.source instanceof NativeAudioSource) {
			entry.from = makeTail(a.source);
		} else {
			a.source.destroy();
		}
		join = entry;
		if (b.source.isReady()) {
			entry.limit = setTimeout(() => {
				if (join !== entry) return;
				refuse(b);
			}, JOIN_LIMIT_MS);
		}
		b.source.play();
		if (fade === null) settle();
		return true;
	}

	/** B did not start: the player loads it again, on the deck that played last. */
	function refuse(b: Preload): void {
		clearJoin();
		hooks.refused(b);
	}

	/** B goes to full, over `seconds` while the engine runs, and the song it joined from ends. */
	function settle(seconds = 0): void {
		const b = current?.source;
		const engine = tryGetAudioEngine();
		if (engine && b instanceof NativeAudioSource) {
			engine.fade(b.lease.deck, 1, engine.running ? seconds : 0, 'linear');
		}
		for (const tail of tails) fadeOut(tail, CUT_S, TAIL_GRACE_MS);
	}

	/** B made its first sound: the fade starts now, on the engine's clock. */
	function joined(source: NativeAudioSource): void {
		if (join?.source !== source) return;
		const { fade, from } = join;
		clearJoin();
		const engine = runningEngine();
		if (fade === null || !engine || !from || !tails.includes(from)) return settle();
		const seconds = fade > 0 ? fade : joinSeconds(from.source);
		const curve = fade > 0 ? 'power' : 'linear';
		engine.fade(from.source.lease.deck, 0, seconds, curve);
		engine.fade(source.lease.deck, 1, seconds, curve);
		endTailIn(from, seconds * 1000 + TAIL_GRACE_MS);
	}

	return {
		/** The track loaded ahead, or null. */
		get preload(): Preload | null {
			return next;
		},

		/** A fading song still holds a deck. */
		hasTail(): boolean {
			return tails.length > 0;
		},

		/**
		 * Resolves once a deck is free for a load, or null when one is now:
		 * fading songs hold both decks for a few milliseconds after a skip.
		 * `everyDeck` waits until no song fades, so a refused track gets the
		 * deck that played last, which the browser already lets play.
		 */
		waitForDeck(everyDeck = false): Promise<void> | null {
			const enough = () => tails.length < (everyDeck ? 1 : DECKS);
			if (enough()) return null;
			return new Promise((resolve) => deckWaiters.push({ enough, resolve }));
		},

		/** The sound edges of `source`, the track playing, came in. */
		timed(source: PlaybackSource, edges: SoundEdges | null): void {
			if (current?.source === source) current = { ...current, edges };
		},

		/**
		 * The earliest position of the track playing, `length` long, that the
		 * move to the next track can start at: it is loaded ahead of that.
		 */
		earliestStart(length: number): number {
			const edges = current ? edgesFor(current) : null;
			return earliestStart(length, crossfade.seconds, edges);
		},

		/** The player loaded `playing` itself (not through a move): it plays at full. */
		follow(playing: Playing): void {
			current = playing;
			if (playing.source instanceof NativeAudioSource) {
				tryGetAudioEngine()?.holdFade(playing.source.lease.deck, 1);
			}
		},

		/** Takes the next track, loading on the idle deck, to start when its time comes. */
		prepare(preload: Preload): void {
			dropPreload();
			next = preload;
			tryGetAudioEngine()?.holdFade(preload.source.lease.deck, 0);
			preload.source.onError((error) => {
				// Refused at its turn: loaded again, not failed. This runs before the
				// player hears the error, and the new load leaves that one stale.
				if (join?.source === preload.source && error.code === AUTOPLAY_BLOCKED) {
					return refuse(preload);
				}
				if (next !== preload) return;
				console.warn('The next track could not be loaded ahead', error);
				dropPreload();
			});
			preload.source.onStateChange((state) => {
				if (state === 'playing') joined(preload.source);
			});
		},

		/** Drops the track loaded ahead, freeing its deck (and any transcode it held). */
		drop(): void {
			dropPreload();
		},

		/** A's position: while A plays, the start is timed from the latest one, in its last moments. */
		progress(position: number, length: number): void {
			clearArm();
			const a = current;
			const b = next;
			if (!a || !b || !isPlayingNative(a.source) || !tryGetAudioEngine() || length <= 0) return;
			const move = plan(a, b, position, length);
			if (move === null) return;
			const wait = move.at - position;
			if (wait > ARM_WINDOW_S) return;
			armTimer = setTimeout(
				() => {
					armTimer = null;
					start(move.fade);
				},
				Math.max(0, wait * 1000)
			);
		},

		/** A paused for data: nothing starts until it plays again. */
		hold(): void {
			clearArm();
		},

		/**
		 * A listener paused or sought: no start is armed from the old position,
		 * and a fade under way ends, with the song the bar shows at full.
		 */
		interrupt(): void {
			clearArm();
			if (!join && tails.length === 0) return;
			clearJoin();
			settle(CUT_S);
		},

		/** A ended. True when B took over; false leaves the move to the player. */
		ended(): boolean {
			return start(null);
		},

		/**
		 * Ends everything this module holds: the preload, any tail (in a few
		 * milliseconds) and the current track, which fades out over a little
		 * longer while heard. A load waits on `waitForDeck` while both fade.
		 */
		reset(): void {
			clearArm();
			clearJoin();
			dropPreload();
			for (const tail of tails) fadeOut(tail, QUICK_CUT_S, QUICK_GRACE_MS);
			const outgoing = current?.source;
			current = null;
			if (!outgoing) return;
			if (outgoing instanceof NativeAudioSource && runningEngine() && outgoing.isAudible()) {
				fadeOut(makeTail(outgoing), CUT_S, TAIL_GRACE_MS);
			} else {
				outgoing.destroy();
			}
		},

		/** The volume reaches the preloaded track and the tails too. */
		setVolume(level: number): void {
			next?.source.setVolume(level);
			for (const tail of tails) tail.source.setVolume(level);
		}
	};
}

export type Transition = ReturnType<typeof createTransition>;
