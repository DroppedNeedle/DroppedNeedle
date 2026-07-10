import type { PlaybackSource, PlaybackState } from './types';
import { getAudioElement, resumeAudioEngine } from './audioElement';

const LOAD_TIMEOUT_MS = 15_000;
const STALL_TIMEOUT_MS = 15_000;

type NativeSourceType = 'jellyfin' | 'local' | 'navidrome' | 'plex';

export class NativeAudioSource implements PlaybackSource {
	readonly type: NativeSourceType;

	private readonly audio: HTMLAudioElement;
	private readonly url: string;
	private readonly seekable: boolean;

	private stateCallbacks: ((state: PlaybackState) => void)[] = [];
	private readyCallbacks: (() => void)[] = [];
	private errorCallbacks: ((error: { code: string; message: string }) => void)[] = [];
	private progressCallbacks: ((currentTime: number, duration: number) => void)[] = [];

	private listeners: Array<{ event: string; handler: EventListener }> = [];
	private stallTimeoutHandle: ReturnType<typeof setTimeout> | null = null;
	private pendingVolume = 75;
	private destroyed = false;
	private currentState: PlaybackState = 'idle';

	constructor(type: NativeSourceType, opts: { url: string; seekable: boolean }) {
		this.type = type;
		this.url = opts.url;
		this.seekable = opts.seekable;
		this.audio = getAudioElement();
	}

	async load(_info?: unknown): Promise<void> {
		this.destroyed = false;
		this.cleanupListeners();
		this.clearStallTimeout();
		this.emitStateChange('loading');

		await new Promise<void>((resolve, reject) => {
			let settled = false;

			const finalize = (action: () => void): void => {
				if (settled || this.destroyed) return;
				settled = true;
				action();
			};

			const emitProgress = () => {
				const currentTime = this.getCurrentTime();
				const duration = this.getDuration();
				this.progressCallbacks.forEach((cb) => cb(currentTime, duration));
			};

			const onReady = () => {
				finalize(() => {
					emitProgress();
					this.readyCallbacks.forEach((cb) => cb());
					resolve();
				});
			};

			const onPlay = () => {
				this.clearStallTimeout();
				this.emitStateChange('playing');
			};

			const onPlaying = () => {
				this.clearStallTimeout();
				if (this.currentState !== 'playing') {
					this.emitStateChange('playing');
				}
			};

			const onPause = () => {
				if (this.audio.ended) return;
				this.emitStateChange('paused');
			};

			const onEnded = () => {
				this.emitStateChange('ended');
			};

			const onWaiting = () => {
				this.emitStateChange('buffering');
				this.startStallTimeout();
			};

			const onTimeUpdate = () => {
				this.clearStallTimeout();
				if (this.currentState === 'buffering') {
					this.emitStateChange('playing');
				}
				emitProgress();
			};

			const onError = () => {
				const code = this.audio.error?.code ?? 0;
				const message = this.getMediaErrorMessage(code);
				this.emitStateChange('error');
				this.emitError('LOAD_ERROR', message);
				finalize(() => reject(new Error(message)));
			};

			const onStalled = () => {
				this.startStallTimeout();
			};

			const timeoutHandle = setTimeout(() => {
				if (settled || this.destroyed) return;
				settled = true;
				const message = `Native audio source load timed out after ${LOAD_TIMEOUT_MS}ms`;
				this.audio.src = '';
				this.cleanupListeners();
				this.clearStallTimeout();
				this.emitError('LOAD_TIMEOUT', message);
				reject(new Error(message));
			}, LOAD_TIMEOUT_MS);

			const readyEvents = ['canplay', 'loadedmetadata', 'loadeddata'];
			const handleReady = () => {
				clearTimeout(timeoutHandle);
				// Single-shot: detach all ready listeners on first fire so the other
				// two events can never re-enter onReady.
				for (const event of readyEvents) {
					this.unregisterListener(event, handleReady);
				}
				onReady();
			};
			for (const event of readyEvents) {
				this.registerListener(event, handleReady);
			}
			this.registerListener('durationchange', emitProgress);
			this.registerListener('play', onPlay);
			this.registerListener('playing', onPlaying);
			this.registerListener('pause', onPause);
			this.registerListener('ended', onEnded);
			this.registerListener('waiting', onWaiting);
			this.registerListener('timeupdate', onTimeUpdate);
			this.registerListener('error', () => {
				clearTimeout(timeoutHandle);
				onError();
			});
			this.registerListener('stalled', onStalled);

			this.audio.src = this.url;
			this.audio.volume = this.pendingVolume / 100;
			this.audio.load();
		});
	}

	play(): void {
		void this.playWithEngineResume();
	}

	pause(): void {
		this.audio.pause();
	}

	seekTo(seconds: number): void {
		if (!this.seekable) {
			return;
		}
		const clamped = Math.max(0, seconds);
		const dur = this.getDuration();
		this.audio.currentTime = dur > 0 ? Math.min(clamped, dur) : clamped;
	}

	setVolume(level: number): void {
		const clamped = Math.max(0, Math.min(100, level));
		this.pendingVolume = clamped;
		if (this.audio.src) {
			this.audio.volume = clamped / 100;
		}
	}

	getCurrentTime(): number {
		const current = this.audio.currentTime;
		return Number.isFinite(current) ? current : 0;
	}

	getDuration(): number {
		const total = this.audio.duration;
		return Number.isFinite(total) ? total : 0;
	}

	destroy(): void {
		this.destroyed = true;
		this.clearStallTimeout();
		this.cleanupListeners();
		// Fully disarm the shared element: pause and drop the src attribute so
		// lock-screen/OS play events cannot restart a zombie stream after stop().
		this.audio.pause();
		this.audio.src = '';
		if (typeof this.audio.removeAttribute === 'function') {
			this.audio.removeAttribute('src');
		}
		this.audio.load();
		this.stateCallbacks = [];
		this.readyCallbacks = [];
		this.errorCallbacks = [];
		this.progressCallbacks = [];
	}

	onStateChange(callback: (state: PlaybackState) => void): void {
		this.stateCallbacks.push(callback);
	}

	onReady(callback: () => void): void {
		this.readyCallbacks.push(callback);
	}

	onError(callback: (error: { code: string; message: string }) => void): void {
		this.errorCallbacks.push(callback);
	}

	onProgress(callback: (currentTime: number, duration: number) => void): void {
		this.progressCallbacks.push(callback);
	}

	isSeekable(): boolean {
		return this.seekable;
	}

	private registerListener(event: string, handler: EventListener): void {
		this.audio.addEventListener(event, handler);
		this.listeners.push({ event, handler });
	}

	private unregisterListener(event: string, handler: EventListener): void {
		this.audio.removeEventListener(event, handler);
		this.listeners = this.listeners.filter((l) => !(l.event === event && l.handler === handler));
	}

	private cleanupListeners(): void {
		for (const { event, handler } of this.listeners) {
			this.audio.removeEventListener(event, handler);
		}
		this.listeners = [];
	}

	private startStallTimeout(): void {
		this.clearStallTimeout();
		this.stallTimeoutHandle = setTimeout(() => {
			if (this.destroyed) return;
			if (typeof document !== 'undefined' && document.hidden) {
				// Background throttling starves media fetches while the tab/PWA is
				// hidden; that stall is benign and must not feed the store's
				// auto-skip chain. Re-arm and re-check once visible again.
				this.startStallTimeout();
				return;
			}
			this.emitError('NETWORK_STALL', `Playback stalled for ${STALL_TIMEOUT_MS}ms`);
			this.emitStateChange('error');
		}, STALL_TIMEOUT_MS);
	}

	private clearStallTimeout(): void {
		if (!this.stallTimeoutHandle) return;
		clearTimeout(this.stallTimeoutHandle);
		this.stallTimeoutHandle = null;
	}

	private async playWithEngineResume(): Promise<void> {
		try {
			await resumeAudioEngine();
		} catch {
			// Keep native playback attempt alive even if Web Audio resume fails.
		}
		try {
			await this.audio.play();
		} catch {
			this.emitError('AUTOPLAY_BLOCKED', 'Playback failed. Browser may be blocking autoplay.');
			this.emitStateChange('error');
		}
	}

	private emitStateChange(state: PlaybackState): void {
		this.currentState = state;
		this.stateCallbacks.forEach((cb) => cb(state));
	}

	private emitError(code: string, message: string): void {
		this.errorCallbacks.forEach((cb) => cb({ code, message }));
	}

	private getMediaErrorMessage(code: number): string {
		switch (code) {
			case 1:
				return 'MEDIA_ERR_ABORTED: Playback was aborted';
			case 2:
				return 'MEDIA_ERR_NETWORK: A network error occurred';
			case 3:
				return 'MEDIA_ERR_DECODE: Decoding failed due to corruption or unsupported features';
			case 4:
				return 'MEDIA_ERR_SRC_NOT_SUPPORTED: Audio source is not supported';
			default:
				return 'Unknown media error';
		}
	}
}
