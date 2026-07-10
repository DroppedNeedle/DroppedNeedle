import { beforeEach, describe, expect, it, vi } from 'vitest';

const mockEngine = vi.hoisted(() => ({
	connect: vi.fn(),
	destroy: vi.fn(),
	isConnected: vi.fn(() => true),
	resume: vi.fn(async () => undefined),
	onContextStateChange: vi.fn()
}));

vi.mock('./audioEngine', () => {
	const MockAudioEngine = vi.fn().mockImplementation(() => mockEngine);
	return { AudioEngine: MockAudioEngine };
});

import {
	_resetAudioElement,
	getAudioElement,
	getAudioEngine,
	onAudioEngineSuspended,
	resumeAudioEngine,
	tryGetAudioEngine,
	setAudioElement
} from './audioElement';

describe('audioElement registry', () => {
	beforeEach(() => {
		_resetAudioElement();
		vi.clearAllMocks();
		mockEngine.resume.mockResolvedValue(undefined);
	});

	it('throws when getting audio element before registration', () => {
		expect.assertions(1);
		expect(() => getAudioElement()).toThrow('Audio element not mounted');
	});

	it('returns registered audio element', () => {
		expect.assertions(1);
		const audio = { src: '' } as HTMLAudioElement;
		setAudioElement(audio);
		expect(getAudioElement()).toBe(audio);
	});

	it('allows replacing the registered audio element', () => {
		expect.assertions(1);
		const first = { src: '' } as HTMLAudioElement;
		const second = { src: '' } as HTMLAudioElement;
		setAudioElement(first);
		setAudioElement(second);
		expect(getAudioElement()).toBe(second);
	});

	it('is idempotent for the same element', () => {
		expect.assertions(1);
		const audio = { src: '' } as HTMLAudioElement;
		setAudioElement(audio);
		const engineFirst = tryGetAudioEngine();
		setAudioElement(audio);
		const engineSecond = tryGetAudioEngine();
		expect(engineFirst).toBe(engineSecond);
	});

	it('throws getAudioEngine before registration', () => {
		expect.assertions(1);
		expect(() => getAudioEngine()).toThrow('Audio engine not initialized');
	});

	it('returns engine after setAudioElement', () => {
		expect.assertions(2);
		const audio = { src: '' } as HTMLAudioElement;
		setAudioElement(audio);
		const engine = getAudioEngine();
		expect(engine).toBeDefined();
		expect(engine.connect).toBeDefined();
	});

	it('tryGetAudioEngine returns null before registration', () => {
		expect.assertions(1);
		expect(tryGetAudioEngine()).toBeNull();
	});

	it('tryGetAudioEngine returns engine after registration', () => {
		expect.assertions(1);
		const audio = { src: '' } as HTMLAudioElement;
		setAudioElement(audio);
		expect(tryGetAudioEngine()).not.toBeNull();
	});

	it('_resetAudioElement destroys engine', () => {
		expect.assertions(2);
		const audio = { src: '' } as HTMLAudioElement;
		setAudioElement(audio);
		const engine = getAudioEngine();
		_resetAudioElement();
		expect(engine.destroy).toHaveBeenCalled();
		expect(tryGetAudioEngine()).toBeNull();
	});

	it('resumeAudioEngine calls the registered engine', async () => {
		expect.assertions(1);
		const audio = { src: '' } as HTMLAudioElement;
		setAudioElement(audio);

		await resumeAudioEngine();

		expect(mockEngine.resume).toHaveBeenCalledTimes(1);
	});

	it('resumeAudioEngine is a no-op when the engine is unavailable', async () => {
		expect.assertions(1);

		await resumeAudioEngine();

		expect(mockEngine.resume).not.toHaveBeenCalled();
	});

	it('resumeAudioEngine swallows browser resume rejections', async () => {
		expect.assertions(2);
		const audio = { src: '' } as HTMLAudioElement;
		setAudioElement(audio);
		mockEngine.resume.mockRejectedValueOnce(new Error('not allowed'));

		await expect(resumeAudioEngine()).resolves.toBeUndefined();
		expect(mockEngine.resume).toHaveBeenCalledTimes(1);
	});

	it('sets navigator.audioSession.type to playback when available (iOS 17+)', () => {
		expect.assertions(1);
		const audioSession = { type: 'auto' };
		vi.stubGlobal('navigator', { audioSession });

		setAudioElement({ src: '' } as HTMLAudioElement);

		expect(audioSession.type).toBe('playback');
		vi.unstubAllGlobals();
	});

	it('registers without audioSession support (no throw)', () => {
		expect.assertions(1);
		vi.stubGlobal('navigator', {});

		setAudioElement({ src: '' } as HTMLAudioElement);

		expect(getAudioElement()).toBeDefined();
		vi.unstubAllGlobals();
	});

	it('notifies suspend listeners when the engine context suspends', () => {
		expect.assertions(3);
		const listener = vi.fn();
		const unsubscribe = onAudioEngineSuspended(listener);
		setAudioElement({ src: '' } as HTMLAudioElement);
		const bridge = mockEngine.onContextStateChange.mock.calls[0][0] as (state: string) => void;

		bridge('running');
		expect(listener).not.toHaveBeenCalled();

		bridge('suspended');
		expect(listener).toHaveBeenCalledTimes(1);

		unsubscribe();
		bridge('suspended');
		expect(listener).toHaveBeenCalledTimes(1);
	});
});
