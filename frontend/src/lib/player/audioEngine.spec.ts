import { describe, it, expect, vi, beforeEach } from 'vitest';

function createMockFilter() {
	return {
		type: '' as string,
		frequency: { value: 0 },
		Q: { value: 0 },
		gain: { value: 0 },
		connect: vi.fn(),
		disconnect: vi.fn()
	};
}

function createMockSource() {
	return {
		connect: vi.fn(),
		disconnect: vi.fn()
	};
}

function createMockAnalyser() {
	return {
		fftSize: 0,
		frequencyBinCount: 64,
		smoothingTimeConstant: 0,
		connect: vi.fn(),
		disconnect: vi.fn(),
		getByteFrequencyData: vi.fn()
	};
}

function createMockContext(
	mockSource: ReturnType<typeof createMockSource>,
	mockFilterFactory: () => ReturnType<typeof createMockFilter>,
	mockAnalyser: ReturnType<typeof createMockAnalyser>
) {
	return {
		state: 'suspended' as string,
		destination: {},
		createMediaElementSource: vi.fn(() => mockSource),
		createBiquadFilter: vi.fn(mockFilterFactory),
		createAnalyser: vi.fn(() => mockAnalyser),
		resume: vi.fn(() => Promise.resolve()),
		close: vi.fn(() => Promise.resolve())
	};
}

vi.stubGlobal('AudioContext', vi.fn());

import { AudioEngine } from './audioEngine';

describe('AudioEngine', () => {
	let engine: AudioEngine;
	let mockSource: ReturnType<typeof createMockSource>;
	let mockFilters: ReturnType<typeof createMockFilter>[];
	let mockAnalyser: ReturnType<typeof createMockAnalyser>;
	let mockCtx: ReturnType<typeof createMockContext>;
	const mockAudio = { src: '' } as unknown as HTMLAudioElement;

	beforeEach(() => {
		vi.clearAllMocks();
		engine = new AudioEngine();
		mockSource = createMockSource();
		mockFilters = [];
		mockAnalyser = createMockAnalyser();

		const filterFactory = () => {
			const f = createMockFilter();
			mockFilters.push(f);
			return f;
		};

		mockCtx = createMockContext(mockSource, filterFactory, mockAnalyser);
		vi.mocked(AudioContext).mockImplementation(() => mockCtx as unknown as AudioContext);
	});

	describe('connect', () => {
		it('creates context, source, and 10 filters wired in chain', () => {
			expect.assertions(6);
			engine.connect(mockAudio);

			expect(mockCtx.createMediaElementSource).toHaveBeenCalledWith(mockAudio);
			expect(mockCtx.createBiquadFilter).toHaveBeenCalledTimes(10);
			expect(mockFilters).toHaveLength(10);
			expect(mockSource.connect).toHaveBeenCalledWith(mockFilters[0]);
			expect(mockFilters[8].connect).toHaveBeenCalledWith(mockFilters[9]);
			expect(mockFilters[9].connect).toHaveBeenCalledWith(mockCtx.destination);
		});

		it('sets correct frequencies and Q on filters', () => {
			expect.assertions(3);
			engine.connect(mockAudio);

			expect(mockFilters[0].frequency.value).toBe(31);
			expect(mockFilters[9].frequency.value).toBe(16000);
			expect(mockFilters[0].Q.value).toBe(1.4);
		});

		it('is idempotent for same element', () => {
			expect.assertions(1);
			engine.connect(mockAudio);
			engine.connect(mockAudio);

			expect(mockCtx.createMediaElementSource).toHaveBeenCalledTimes(1);
		});

		it('destroys and reconnects for a different element', () => {
			expect.assertions(2);
			engine.connect(mockAudio);
			const otherAudio = { src: 'other' } as unknown as HTMLAudioElement;

			engine.connect(otherAudio);

			expect(mockSource.disconnect).toHaveBeenCalled();
			expect(AudioContext).toHaveBeenCalledTimes(2);
		});
	});

	describe('setBandGain', () => {
		it('sets gain on the correct filter', () => {
			expect.assertions(1);
			engine.connect(mockAudio);
			engine.setBandGain(3, 6);

			expect(mockFilters[3].gain.value).toBe(6);
		});

		it('clamps gain to [-12, 12]', () => {
			expect.assertions(2);
			engine.connect(mockAudio);
			engine.setBandGain(0, 20);
			expect(mockFilters[0].gain.value).toBe(12);

			engine.setBandGain(0, -20);
			expect(mockFilters[0].gain.value).toBe(-12);
		});

		it('ignores out-of-range index', () => {
			expect.assertions(1);
			engine.connect(mockAudio);
			engine.setBandGain(15, 5);
			engine.setBandGain(-1, 5);

			expect(mockFilters.every((f) => f.gain.value === 0)).toBe(true);
		});
	});

	describe('setAllGains', () => {
		it('sets all 10 filter gains', () => {
			expect.assertions(2);
			engine.connect(mockAudio);
			const gains = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
			engine.setAllGains(gains);

			expect(mockFilters[0].gain.value).toBe(1);
			expect(mockFilters[9].gain.value).toBe(10);
		});
	});

	describe('setEnabled', () => {
		it('zeros all gains when disabled', () => {
			expect.assertions(1);
			engine.connect(mockAudio);
			engine.setAllGains([5, 5, 5, 5, 5, 5, 5, 5, 5, 5]);
			engine.setEnabled(false, [5, 5, 5, 5, 5, 5, 5, 5, 5, 5]);

			expect(mockFilters.every((f) => f.gain.value === 0)).toBe(true);
		});

		it('restores stored gains when enabled', () => {
			expect.assertions(2);
			engine.connect(mockAudio);
			const stored = [3, -2, 1, 0, 4, -1, 2, 5, -3, 6];
			engine.setEnabled(true, stored);

			expect(mockFilters[0].gain.value).toBe(3);
			expect(mockFilters[9].gain.value).toBe(6);
		});
	});

	describe('resume', () => {
		it('calls context.resume when suspended', async () => {
			expect.assertions(1);
			engine.connect(mockAudio);
			mockCtx.state = 'suspended';
			await engine.resume();

			expect(mockCtx.resume).toHaveBeenCalled();
		});

		it('does not call resume when already running', async () => {
			expect.assertions(1);
			engine.connect(mockAudio);
			mockCtx.state = 'running';
			await engine.resume();

			expect(mockCtx.resume).not.toHaveBeenCalled();
		});
	});

	describe('isConnected', () => {
		it('returns false before connect', () => {
			expect.assertions(1);
			expect(engine.isConnected()).toBe(false);
		});

		it('returns true after connect', () => {
			expect.assertions(1);
			engine.connect(mockAudio);
			expect(engine.isConnected()).toBe(true);
		});
	});

	describe('destroy', () => {
		it('disconnects all nodes and closes context', () => {
			expect.assertions(4);
			engine.connect(mockAudio);
			engine.destroy();

			expect(mockSource.disconnect).toHaveBeenCalled();
			expect(mockFilters[0].disconnect).toHaveBeenCalled();
			expect(mockCtx.close).toHaveBeenCalled();
			expect(engine.isConnected()).toBe(false);
		});
	});

	describe('onContextStateChange', () => {
		it('notifies listeners when the AudioContext state flips', () => {
			expect.assertions(2);
			const cb = vi.fn();
			engine.onContextStateChange(cb);
			engine.connect(mockAudio);
			const ctx = mockCtx as unknown as { onstatechange: () => void };

			mockCtx.state = 'suspended';
			ctx.onstatechange();
			expect(cb).toHaveBeenCalledWith('suspended');

			mockCtx.state = 'running';
			ctx.onstatechange();
			expect(cb).toHaveBeenCalledWith('running');
		});

		it('clears the context handler on destroy', () => {
			expect.assertions(1);
			engine.connect(mockAudio);
			engine.destroy();
			expect(
				(mockCtx as unknown as { onstatechange: (() => void) | null }).onstatechange
			).toBeNull();
		});
	});

	describe('getFrequencies', () => {
		it('returns the 10 standard frequencies', () => {
			expect.assertions(2);
			const freqs = engine.getFrequencies();
			expect(freqs).toHaveLength(10);
			expect(freqs[0]).toBe(31);
		});
	});

	describe('analyser', () => {
		it('creates an analyser tapped off the end of the filter chain', () => {
			expect.assertions(4);
			engine.connect(mockAudio);

			expect(mockCtx.createAnalyser).toHaveBeenCalledTimes(1);
			expect(mockAnalyser.fftSize).toBe(128);
			expect(mockFilters[9].connect).toHaveBeenCalledWith(mockAnalyser);
			// Analyser must be a terminal sink, else audio sums to destination twice.
			expect(mockAnalyser.connect).not.toHaveBeenCalled();
		});

		it('getFrequencyData returns null before connect and a buffer after', () => {
			expect.assertions(3);
			expect(engine.getFrequencyData()).toBeNull();

			engine.connect(mockAudio);
			const data = engine.getFrequencyData();

			expect(data).toBeInstanceOf(Uint8Array);
			expect(mockAnalyser.getByteFrequencyData).toHaveBeenCalled();
		});

		it('disconnects the analyser on destroy', () => {
			expect.assertions(2);
			engine.connect(mockAudio);
			engine.destroy();

			expect(mockAnalyser.disconnect).toHaveBeenCalled();
			expect(engine.getFrequencyData()).toBeNull();
		});
	});
});
