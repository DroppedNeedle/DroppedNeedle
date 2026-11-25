import { AudioEngine } from './audioEngine';

// A clip of silence, played once on each deck at the first click or key so
// Safari lets that element play later without one.
const SILENT_CLIP =
	'data:audio/wav;base64,UklGRjQAAABXQVZFZm10IBAAAAABAAEAQB8AAIA+AAACABAAZGF0YRAAAAAAAAAAAAAAAAAAAAAAAAAA';

/**
 * One of the player's two `<audio>` decks, held by one source at a time. A
 * lease from before the deck was released or claimed again no longer holds
 * it, so a stale source can never blank the deck another one plays on.
 */
export interface DeckLease {
	readonly element: HTMLAudioElement;
	/** The deck's place in the audio engine. */
	readonly deck: number;
	held(): boolean;
	/** The element played: the browser lets this deck play again without a click. */
	played(): void;
	release(): void;
}

interface Deck {
	element: HTMLAudioElement;
	generation: number;
	held: boolean;
	// When the deck last started playing, by a counter; 0 before it ever has.
	playedAt: number;
}

let decks: Deck[] = [];
let plays = 0;
let engine: AudioEngine | null = null;

// Lock screen Play after a call reaches the element, not the player: a deck
// that starts while the engine is stopped wakes it.
function onDeckPlaying(): void {
	if (engine && !engine.running) void resumeAudioEngine();
}

/** Mounts the decks and wires them through the audio engine. */
export function setAudioDecks(elements: readonly HTMLAudioElement[]): void {
	const same =
		elements.length === decks.length && elements.every((el, i) => decks[i].element === el);
	if (same && engine) return;
	engine?.destroy();
	engine = null;
	for (const deck of decks) deck.element.removeEventListener('playing', onDeckPlaying);
	for (const element of elements) element.addEventListener('playing', onDeckPlaying);
	decks = elements.map((element) => ({ element, generation: 0, held: false, playedAt: 0 }));
	plays = 0;
	try {
		const newEngine = new AudioEngine();
		newEngine.connect(elements);
		engine = newEngine;
	} catch {
		// connect() can throw (InvalidStateError, SecurityError).
		// The decks still play without EQ or fades - engine stays null.
	}
}

/**
 * Claims a free deck: the one that played last, which the browser already
 * lets play, else the first. Throws while none is mounted or free.
 */
export function claimDeck(): DeckLease {
	if (decks.length === 0) {
		throw new Error('Audio decks not mounted: setAudioDecks() must be called before playback');
	}
	const free = decks.filter((d) => !d.held);
	if (free.length === 0) throw new Error('No free audio deck');
	const deck = free.reduce((best, d) => (d.playedAt > best.playedAt ? d : best));
	deck.held = true;
	const generation = ++deck.generation;
	const held = () => deck.held && deck.generation === generation;
	return {
		element: deck.element,
		deck: decks.indexOf(deck),
		held,
		played: () => {
			if (held()) deck.playedAt = ++plays;
		},
		release: () => {
			if (!held()) return;
			deck.held = false;
			deck.generation++;
		}
	};
}

/**
 * Plays the silent clip on every deck no source holds, inside the first
 * click or key, so each may play later without one. A deck claimed
 * meanwhile is left to its new owner.
 */
export function unlockIdleDecks(): void {
	for (const deck of decks) {
		if (deck.held) continue;
		const generation = deck.generation;
		const el = deck.element;
		const settle = () => {
			el.muted = false;
			if (deck.held || deck.generation !== generation) return;
			el.pause();
			el.removeAttribute('src');
			el.load();
		};
		el.muted = true;
		el.src = SILENT_CLIP;
		try {
			void el.play().then(settle, settle);
		} catch {
			settle();
		}
	}
}

export function tryGetAudioEngine(): AudioEngine | null {
	return engine;
}

export async function resumeAudioEngine(): Promise<void> {
	try {
		await engine?.resume();
	} catch {
		// Browsers can reject resume() outside a user activation. Native audio
		// playback should still continue; the next user gesture can retry.
	}
}
