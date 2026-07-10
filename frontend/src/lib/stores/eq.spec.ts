import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

const mockEngine = {
	setAllGains: vi.fn(),
	setEnabled: vi.fn(),
	isConnected: vi.fn(() => true)
};

vi.mock('$lib/player/audioElement', () => ({
	tryGetAudioEngine: vi.fn(() => mockEngine)
}));

const STORAGE_KEY = 'droppedneedle_eq_settings';

const storage = new Map<string, string>();
const mockLocalStorage = {
	getItem: vi.fn((key: string) => storage.get(key) ?? null),
	setItem: vi.fn((key: string, value: string) => storage.set(key, value)),
	removeItem: vi.fn((key: string) => storage.delete(key)),
	clear: vi.fn(() => storage.clear()),
	get length() {
		return storage.size;
	},
	key: vi.fn((_i: number) => null)
};

vi.stubGlobal('localStorage', mockLocalStorage);
vi.stubGlobal('window', globalThis);

describe('eqStore', () => {
	let eqStore: (typeof import('./eq.svelte'))['eqStore'];

	beforeEach(async () => {
		vi.clearAllMocks();
		storage.clear();
		vi.resetModules();
		// Import before faking timers: the vite module runner's transport retries on
		// real setTimeout, so a first fetch under fake timers can stall the hook.
		const mod = await import('./eq.svelte');
		eqStore = mod.eqStore;
		vi.useFakeTimers();
	});

	afterEach(() => {
		vi.runOnlyPendingTimers();
		vi.useRealTimers();
		storage.clear();
	});

	it('initializes with defaults when no localStorage data', () => {
		expect.assertions(3);
		expect(eqStore.enabled).toBe(false);
		expect(eqStore.gains).toEqual([0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
		expect(eqStore.activePreset).toBe('Flat');
	});

	it('restores valid settings from localStorage', async () => {
		expect.assertions(3);
		const stored = { enabled: true, gains: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10], activePreset: 'Rock' };
		storage.set(STORAGE_KEY, JSON.stringify(stored));
		vi.resetModules();
		const mod = await import('./eq.svelte');
		const store = mod.eqStore;

		expect(store.enabled).toBe(true);
		expect(store.gains).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
		expect(store.activePreset).toBe('Rock');
	});

	it('falls back to defaults on malformed localStorage data', async () => {
		expect.assertions(2);
		storage.set(STORAGE_KEY, '{"enabled":"notabool"}');
		vi.resetModules();
		const mod = await import('./eq.svelte');
		const store = mod.eqStore;

		expect(store.enabled).toBe(false);
		expect(store.gains).toEqual([0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
	});

	it('clamps out-of-range gains from localStorage', async () => {
		expect.assertions(2);
		const stored = { enabled: false, gains: [20, -20, 0, 0, 0, 0, 0, 0, 0, 0], activePreset: null };
		storage.set(STORAGE_KEY, JSON.stringify(stored));
		vi.resetModules();
		const mod = await import('./eq.svelte');
		const store = mod.eqStore;

		expect(store.gains[0]).toBe(12);
		expect(store.gains[1]).toBe(-12);
	});

	it('toggleEq flips enabled and syncs to engine', () => {
		expect.assertions(2);
		eqStore.toggleEq();
		expect(eqStore.enabled).toBe(true);
		expect(mockEngine.setAllGains).toHaveBeenCalled();
	});

	it('setBandGain updates a single band and clamps', () => {
		expect.assertions(2);
		eqStore.setBandGain(3, 15);
		expect(eqStore.gains[3]).toBe(12);
		eqStore.setBandGain(3, -15);
		expect(eqStore.gains[3]).toBe(-12);
	});

	it('setBandGain ignores out-of-range index', () => {
		expect.assertions(1);
		eqStore.setBandGain(15, 5);
		expect(eqStore.gains).toEqual([0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
	});

	it('applyPreset sets all gains to preset values', () => {
		expect.assertions(2);
		eqStore.applyPreset('Rock');
		expect(eqStore.gains).toEqual([5, 4, 3, 1, -1, 1, 3, 4, 5, 5]);
		expect(eqStore.activePreset).toBe('Rock');
	});

	it('resetToFlat applies the Flat preset', () => {
		expect.assertions(2);
		eqStore.applyPreset('Rock');
		eqStore.resetToFlat();
		expect(eqStore.gains).toEqual([0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
		expect(eqStore.activePreset).toBe('Flat');
	});

	it('detects custom when gains do not match any preset', () => {
		expect.assertions(1);
		eqStore.setBandGain(0, 7);
		expect(eqStore.activePreset).toBeNull();
	});

	it('detects matching preset after manual band changes', () => {
		expect.assertions(1);
		const rock = [5, 4, 3, 1, -1, 1, 3, 4, 5, 5];
		for (let i = 0; i < rock.length; i++) {
			eqStore.setBandGain(i, rock[i]);
		}
		expect(eqStore.activePreset).toBe('Rock');
	});

	it('persists to localStorage with debounce', () => {
		expect.assertions(1);
		eqStore.toggleEq();
		vi.advanceTimersByTime(200);
		const stored = storage.get(STORAGE_KEY) ?? null;
		expect(stored).not.toBeNull();
	});
});
