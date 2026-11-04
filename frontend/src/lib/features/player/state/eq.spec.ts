import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

const mockEngine = {
	setAllGains: vi.fn(),
	setEnabled: vi.fn(),
	isConnected: vi.fn(() => true)
};

vi.mock('$lib/features/player/playback/audioElement', () => ({
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

// The first import compiles the store, which on a loaded host can outrun a
// hook's time limit. Loading it here, while the file is collected, leaves
// each test's fresh import only the module's own start.
await import('./eq.svelte');

describe('eqStore', () => {
	let eqStore: (typeof import('./eq.svelte'))['eqStore'];

	beforeEach(async () => {
		vi.useFakeTimers();
		vi.clearAllMocks();
		storage.clear();
		vi.resetModules();
		const mod = await import('./eq.svelte');
		eqStore = mod.eqStore;
	});

	afterEach(() => {
		vi.runOnlyPendingTimers();
		vi.useRealTimers();
		storage.clear();
	});

	it('restores stored settings, defaults for malformed data and clamps out-of-range gains', async () => {
		{
			// Restores valid settings from localStorage.
			const stored = {
				enabled: true,
				gains: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
				activePreset: 'Rock'
			};
			storage.set(STORAGE_KEY, JSON.stringify(stored));
			vi.resetModules();
			const mod = await import('./eq.svelte');
			const store = mod.eqStore;

			expect(store.enabled).toBe(true);
			expect(store.gains).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
			expect(store.activePreset).toBe('Rock');
		}

		{
			// Falls back to defaults on malformed localStorage data.
			storage.set(STORAGE_KEY, '{"enabled":"notabool"}');
			vi.resetModules();
			const mod = await import('./eq.svelte');
			const store = mod.eqStore;

			expect(store.enabled).toBe(false);
			expect(store.gains).toEqual([0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
		}

		{
			// Clamps out-of-range gains from localStorage.
			const stored = {
				enabled: false,
				gains: [20, -20, 0, 0, 0, 0, 0, 0, 0, 0],
				activePreset: null
			};
			storage.set(STORAGE_KEY, JSON.stringify(stored));
			vi.resetModules();
			const mod = await import('./eq.svelte');
			const store = mod.eqStore;

			expect(store.gains[0]).toBe(12);
			expect(store.gains[1]).toBe(-12);
		}
	});

	it('setBandGain updates a single band and clamps', () => {
		expect.assertions(2);
		eqStore.setBandGain(3, 15);
		expect(eqStore.gains[3]).toBe(12);
		eqStore.setBandGain(3, -15);
		expect(eqStore.gains[3]).toBe(-12);
	});
});
