import { EQ_FREQUENCIES, EQ_BAND_COUNT, EQ_MIN_GAIN, EQ_MAX_GAIN } from '../stores/eqPresets';

const DEFAULT_Q = 1.4;
// Small FFT keeps per-frame visualiser reads cheap; 128 yields 64 bins.
const ANALYSER_FFT_SIZE = 128;

export class AudioEngine {
	private context: AudioContext | null = null;
	private source: MediaElementAudioSourceNode | null = null;
	private filters: BiquadFilterNode[] = [];
	private analyser: AnalyserNode | null = null;
	private freqData: Uint8Array<ArrayBuffer> | null = null;
	private connectedElement: HTMLAudioElement | null = null;
	private stateChangeCallbacks: ((state: AudioContextState) => void)[] = [];

	connect(audio: HTMLAudioElement): void {
		if (this.connectedElement === audio) return;
		if (this.connectedElement) {
			this.destroy();
		}

		this.context = new AudioContext();
		// iOS suspends the AudioContext when a PWA backgrounds; surface state flips
		// so callers can resume the chain instead of letting playback die silently.
		this.context.onstatechange = () => {
			const state = this.context?.state;
			if (!state) return;
			for (const cb of this.stateChangeCallbacks) cb(state);
		};
		this.source = this.context.createMediaElementSource(audio);

		this.filters = EQ_FREQUENCIES.map((freq) => {
			const filter = this.context!.createBiquadFilter();
			filter.type = 'peaking';
			filter.frequency.value = freq;
			filter.Q.value = DEFAULT_Q;
			filter.gain.value = 0;
			return filter;
		});

		let prev: AudioNode = this.source;
		for (const filter of this.filters) {
			prev.connect(filter);
			prev = filter;
		}
		prev.connect(this.context.destination);

		// Analyser is a terminal sink (not connected onward), so it never alters the audio.
		if (typeof this.context.createAnalyser === 'function') {
			this.analyser = this.context.createAnalyser();
			this.analyser.fftSize = ANALYSER_FFT_SIZE;
			this.analyser.smoothingTimeConstant = 0.82;
			prev.connect(this.analyser);
		}

		this.connectedElement = audio;
	}

	/**
	 * Current frequency spectrum (0-255 per bin) for the visualiser, or null when
	 * no analyser is available. The buffer is owned and reused across frames.
	 */
	getFrequencyData(): Uint8Array | null {
		if (!this.analyser) return null;
		if (!this.freqData || this.freqData.length !== this.analyser.frequencyBinCount) {
			this.freqData = new Uint8Array(this.analyser.frequencyBinCount);
		}
		this.analyser.getByteFrequencyData(this.freqData);
		return this.freqData;
	}

	setBandGain(index: number, dB: number): void {
		if (index < 0 || index >= EQ_BAND_COUNT || !this.filters[index]) return;
		this.filters[index].gain.value = Math.max(EQ_MIN_GAIN, Math.min(EQ_MAX_GAIN, dB));
	}

	setAllGains(gains: readonly number[]): void {
		for (let i = 0; i < EQ_BAND_COUNT; i++) {
			if (this.filters[i]) {
				this.filters[i].gain.value = Math.max(EQ_MIN_GAIN, Math.min(EQ_MAX_GAIN, gains[i] ?? 0));
			}
		}
	}

	setEnabled(enabled: boolean, storedGains: readonly number[]): void {
		if (enabled) {
			this.setAllGains(storedGains);
		} else {
			for (const filter of this.filters) {
				filter.gain.value = 0;
			}
		}
	}

	getFrequencies(): readonly number[] {
		return EQ_FREQUENCIES;
	}

	isConnected(): boolean {
		return this.connectedElement !== null;
	}

	/** Notifies on AudioContext state flips ('suspended'/'running'/...). */
	onContextStateChange(callback: (state: AudioContextState) => void): void {
		this.stateChangeCallbacks.push(callback);
	}

	async resume(): Promise<void> {
		if (this.context && this.context.state === 'suspended') {
			await this.context.resume();
		}
	}

	destroy(): void {
		for (const filter of this.filters) {
			filter.disconnect();
		}
		this.analyser?.disconnect();
		this.source?.disconnect();
		if (this.context) {
			this.context.onstatechange = null;
			if (this.context.state !== 'closed') {
				void this.context.close();
			}
		}
		this.stateChangeCallbacks = [];
		this.filters = [];
		this.analyser = null;
		this.freqData = null;
		this.source = null;
		this.context = null;
		this.connectedElement = null;
	}
}
