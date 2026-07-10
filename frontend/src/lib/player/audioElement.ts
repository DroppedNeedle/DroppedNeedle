import { AudioEngine } from './audioEngine';

/** Narrow view of the iOS 17+ Audio Session API (not yet in lib.dom). */
interface AudioSessionLike {
	type: string;
}

let audioElement: HTMLAudioElement | null = null;
let engine: AudioEngine | null = null;

const suspendListeners = new Set<() => void>();

/**
 * Subscribe to the engine's AudioContext flipping to 'suspended' (e.g. iOS
 * backgrounding a PWA). Returns an unsubscribe function. Survives engine
 * re-creation because setAudioElement re-bridges each new engine to this set.
 */
export function onAudioEngineSuspended(listener: () => void): () => void {
	suspendListeners.add(listener);
	return () => {
		suspendListeners.delete(listener);
	};
}

function applyAudioSessionHint(): void {
	if (typeof navigator === 'undefined') return;
	const session = (navigator as Navigator & { audioSession?: AudioSessionLike }).audioSession;
	if (!session) return;
	try {
		// Sanctioned iOS Safari/PWA (17+) hint: keep media running in background.
		session.type = 'playback';
	} catch {
		// Hint only — some engines reject unsupported values.
	}
}

export function setAudioElement(el: HTMLAudioElement): void {
	if (audioElement === el && engine) return;
	applyAudioSessionHint();
	if (engine) {
		engine.destroy();
		engine = null;
	}
	audioElement = el;
	try {
		const newEngine = new AudioEngine();
		newEngine.connect(el);
		newEngine.onContextStateChange((state) => {
			if (state !== 'suspended') return;
			for (const listener of suspendListeners) listener();
		});
		engine = newEngine;
	} catch {
		// connect() can throw (InvalidStateError, SecurityError).
		// Audio element is still usable without EQ — engine stays null.
	}
}

export function getAudioElement(): HTMLAudioElement {
	if (!audioElement) {
		throw new Error('Audio element not mounted — setAudioElement() must be called before playback');
	}
	return audioElement;
}

export function getAudioEngine(): AudioEngine {
	if (!engine) {
		throw new Error('Audio engine not initialized — setAudioElement() must be called first');
	}
	return engine;
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

export function _resetAudioElement(): void {
	engine?.destroy();
	engine = null;
	audioElement = null;
}
