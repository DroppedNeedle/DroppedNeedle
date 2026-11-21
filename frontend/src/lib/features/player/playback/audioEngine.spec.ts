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

function createMockGain() {
	return {
		gain: {
			value: 1,
			cancelScheduledValues: vi.fn(),
			setValueAtTime: vi.fn(),
			linearRampToValueAtTime: vi.fn()
		},
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
	mockAnalyser: ReturnType<typeof createMockAnalyser>,
	mockGainFactory: () => ReturnType<typeof createMockGain>
) {
	return {
		state: 'suspended' as string,
		currentTime: 0,
		destination: {},
		addEventListener: vi.fn(),
		createMediaElementSource: vi.fn(() => mockSource),
		createGain: vi.fn(mockGainFactory),
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
	let mockGains: ReturnType<typeof createMockGain>[];
	let mockCtx: ReturnType<typeof createMockContext>;
	const mockAudio = { src: '' } as unknown as HTMLAudioElement;

	beforeEach(() => {
		vi.clearAllMocks();
		engine = new AudioEngine();
		mockSource = createMockSource();
		mockFilters = [];
		mockAnalyser = createMockAnalyser();
		mockGains = [];

		const filterFactory = () => {
			const f = createMockFilter();
			mockFilters.push(f);
			return f;
		};

		const gainFactory = () => {
			const g = createMockGain();
			mockGains.push(g);
			return g;
		};

		mockCtx = createMockContext(mockSource, filterFactory, mockAnalyser, gainFactory);
		vi.mocked(AudioContext).mockImplementation(function () {
			return mockCtx as unknown as AudioContext;
		});
	});

	describe('connect', () => {
		it('wires each deck through its level and fade gains into one EQ, rebuilding it for others', () => {
			expect.assertions(10);
			engine.connect([mockAudio]);

			expect(mockCtx.createMediaElementSource).toHaveBeenCalledWith(mockAudio);
			expect(mockCtx.createBiquadFilter).toHaveBeenCalledTimes(10);
			expect(mockFilters).toHaveLength(10);
			const [level, fade] = mockGains;
			expect(mockSource.connect).toHaveBeenCalledWith(level);
			expect(level.connect).toHaveBeenCalledWith(fade);
			expect(fade.connect).toHaveBeenCalledWith(mockFilters[0]);
			expect(mockFilters[8].connect).toHaveBeenCalledWith(mockFilters[9]);
			expect(mockFilters[9].connect).toHaveBeenCalledWith(mockCtx.destination);

			engine.connect([{ src: 'other' } as unknown as HTMLAudioElement]);

			expect(mockSource.disconnect).toHaveBeenCalled();
			expect(AudioContext).toHaveBeenCalledTimes(2);
		});
	});

	describe('setBandGain', () => {
		it('clamps gain to [-12, 12]', () => {
			expect.assertions(2);
			engine.connect([mockAudio]);
			engine.setBandGain(0, 20);
			expect(mockFilters[0].gain.value).toBe(12);

			engine.setBandGain(0, -20);
			expect(mockFilters[0].gain.value).toBe(-12);
		});
	});

	describe('setEnabled', () => {
		it('zeros every gain when off and restores the stored ones when on', () => {
			expect.assertions(3);
			engine.connect([mockAudio]);
			engine.setAllGains([5, 5, 5, 5, 5, 5, 5, 5, 5, 5]);
			engine.setEnabled(false, [5, 5, 5, 5, 5, 5, 5, 5, 5, 5]);

			expect(mockFilters.every((f) => f.gain.value === 0)).toBe(true);

			const stored = [3, -2, 1, 0, 4, -1, 2, 5, -3, 6];
			engine.setEnabled(true, stored);

			expect(mockFilters[0].gain.value).toBe(3);
			expect(mockFilters[9].gain.value).toBe(6);
		});
	});
});
