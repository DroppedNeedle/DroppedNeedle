import { EQ_FREQUENCIES, EQ_BAND_COUNT, EQ_MIN_GAIN, EQ_MAX_GAIN } from '../state/eqPresets';

const DEFAULT_Q = 1.4;
// Small FFT keeps per-frame visualiser reads cheap; 128 yields 64 bins.
const ANALYSER_FFT_SIZE = 128;
// Straight lines a fade's curve is drawn with.
const CURVE_STEPS = 16;

/**
 * How a fade moves: `linear` keeps the sum of two decks' gains at 1, for a
 * join inside one waveform; `power` keeps the sum of their squares at 1, for
 * a crossfade between two songs.
 */
export type FadeCurve = 'linear' | 'power';

interface Fade {
	from: number;
	to: number;
	start: number;
	seconds: number;
	curve: FadeCurve;
}

interface DeckNodes {
	source: MediaElementAudioSourceNode;
	/** The deck's loudness level; 1 until levelling sets it. */
	level: GainNode;
	/** Fades between tracks, scheduled on the context's clock. */
	fade: GainNode;
	/** The fade last scheduled, so its value can be worked out at any time. */
	plan: Fade;
}

function shape(fade: Fade, x: number): number {
	if (fade.curve === 'linear') return x;
	return fade.to > fade.from ? Math.sin((x * Math.PI) / 2) : 1 - Math.cos((x * Math.PI) / 2);
}

function valueAt(fade: Fade, time: number): number {
	if (fade.seconds <= 0 || time >= fade.start + fade.seconds) return fade.to;
	if (time <= fade.start) return fade.from;
	const x = (time - fade.start) / fade.seconds;
	return fade.from + (fade.to - fade.from) * shape(fade, x);
}

/**
 * Both decks, each through its own level and fade gains, into one 10-band
 * equalizer and on to the speakers, with an analyser on the end for the
 * visualiser.
 */
export class AudioEngine {
	private context: AudioContext | null = null;
	private decks: DeckNodes[] = [];
	private filters: BiquadFilterNode[] = [];
	private analyser: AnalyserNode | null = null;
	private freqData: Uint8Array<ArrayBuffer> | null = null;
	private connected: readonly HTMLAudioElement[] = [];

	connect(elements: readonly HTMLAudioElement[]): void {
		if (
			elements.length === this.connected.length &&
			elements.every((el, i) => this.connected[i] === el)
		) {
			return;
		}
		if (this.connected.length > 0) this.destroy();
		try {
			this.build(elements);
		} catch (error) {
			// A deck that would not connect leaves no half-built graph behind.
			this.destroy();
			throw error;
		}
		this.connected = elements;
	}

	private build(elements: readonly HTMLAudioElement[]): void {
		const context = new AudioContext();
		this.context = context;

		this.filters = EQ_FREQUENCIES.map((freq) => {
			const filter = context.createBiquadFilter();
			filter.type = 'peaking';
			filter.frequency.value = freq;
			filter.Q.value = DEFAULT_Q;
			filter.gain.value = 0;
			return filter;
		});

		for (const element of elements) {
			const source = context.createMediaElementSource(element);
			const level = context.createGain();
			const fade = context.createGain();
			this.decks.push({
				source,
				level,
				fade,
				plan: { from: 1, to: 1, start: 0, seconds: 0, curve: 'linear' }
			});
			source.connect(level);
			level.connect(fade);
			fade.connect(this.filters[0]);
		}

		for (let i = 1; i < this.filters.length; i++) {
			this.filters[i - 1].connect(this.filters[i]);
		}
		const last = this.filters[this.filters.length - 1];
		last.connect(context.destination);

		// Analyser is a terminal sink (not connected onward), so it never alters the audio.
		if (typeof context.createAnalyser === 'function') {
			this.analyser = context.createAnalyser();
			this.analyser.fftSize = ANALYSER_FFT_SIZE;
			this.analyser.smoothingTimeConstant = 0.82;
			last.connect(this.analyser);
		}

		// Away from running, no fade can be heard to its end: each deck goes to where its fade was going.
		context.addEventListener('statechange', () => {
			if (this.running) return;
			for (let i = 0; i < this.decks.length; i++) this.holdFade(i, this.decks[i].plan.to);
		});
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
		return this.connected.length > 0;
	}

	/** Whether the context is playing, so a fade scheduled now is heard. */
	get running(): boolean {
		return this.context?.state === 'running';
	}

	/** Holds a deck's fade gain at `value` from now on, dropping any fade under way. */
	holdFade(deck: number, value: number): void {
		this.fade(deck, value, 0, 'linear');
	}

	/**
	 * Fades a deck from where it is now to `to` over `seconds`. Firefox has no
	 * cancelAndHoldAtTime, so a fade under way is dropped and its value set again.
	 */
	fade(deck: number, to: number, seconds: number, curve: FadeCurve): void {
		const nodes = this.decks[deck];
		if (!nodes || !this.context) return;
		const now = this.context.currentTime;
		const from = valueAt(nodes.plan, now);
		const param = nodes.fade.gain;
		param.cancelScheduledValues(0);
		nodes.plan = { from, to, start: now, seconds, curve };
		if (seconds <= 0) {
			param.setValueAtTime(to, now);
			return;
		}
		param.setValueAtTime(from, now);
		const steps = curve === 'linear' ? 1 : CURVE_STEPS;
		for (let i = 1; i <= steps; i++) {
			const time = now + (seconds * i) / steps;
			param.linearRampToValueAtTime(valueAt(nodes.plan, time), time);
		}
	}

	async resume(): Promise<void> {
		// Safari reports `interrupted` after a call or another app took the sound.
		const state: string | undefined = this.context?.state;
		if (this.context && (state === 'suspended' || state === 'interrupted')) {
			await this.context.resume();
		}
	}

	destroy(): void {
		for (const filter of this.filters) {
			filter.disconnect();
		}
		for (const nodes of this.decks) {
			nodes.source.disconnect();
			nodes.level.disconnect();
			nodes.fade.disconnect();
		}
		this.analyser?.disconnect();
		if (this.context && this.context.state !== 'closed') {
			void this.context.close();
		}
		this.filters = [];
		this.decks = [];
		this.analyser = null;
		this.freqData = null;
		this.context = null;
		this.connected = [];
	}
}
