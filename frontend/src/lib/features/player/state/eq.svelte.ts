import {
	EQ_BAND_COUNT,
	EQ_MIN_GAIN,
	EQ_MAX_GAIN,
	EQ_PRESETS,
	type EqPresetName
} from '$lib/features/player/state/eqPresets';
import { tryGetAudioEngine } from '$lib/features/player/playback/audioElement';

const STORAGE_KEY = 'droppedneedle_eq_settings';
const PERSIST_DEBOUNCE_MS = 150;

interface StoredEqSettings {
	enabled: boolean;
	gains: number[];
	activePreset: string | null;
}

function clampGain(v: number): number {
	return Math.max(EQ_MIN_GAIN, Math.min(EQ_MAX_GAIN, v));
}

function loadFromStorage(): StoredEqSettings | null {
	if (typeof window === 'undefined') return null;
	try {
		const raw = localStorage.getItem(STORAGE_KEY);
		if (!raw) return null;
		const parsed = JSON.parse(raw);
		if (
			typeof parsed !== 'object' ||
			parsed === null ||
			typeof parsed.enabled !== 'boolean' ||
			!Array.isArray(parsed.gains) ||
			parsed.gains.length !== EQ_BAND_COUNT ||
			!parsed.gains.every((v: unknown) => typeof v === 'number' && Number.isFinite(v))
		) {
			return null;
		}
		return {
			enabled: parsed.enabled,
			gains: parsed.gains.map(clampGain),
			activePreset: typeof parsed.activePreset === 'string' ? parsed.activePreset : null
		};
	} catch {
		return null;
	}
}

function createEqStore() {
	const stored = loadFromStorage();

	let enabled = $state(stored?.enabled ?? false);
	let gains = $state<number[]>(stored?.gains ?? Array.from({ length: EQ_BAND_COUNT }, () => 0));
	let activePreset = $state<string | null>(stored?.activePreset ?? 'Flat');
	let persistTimer: ReturnType<typeof setTimeout> | null = null;

	function syncToEngine(): void {
		const engine = tryGetAudioEngine();
		if (!engine) return;
		if (enabled) {
			engine.setAllGains(gains);
		} else {
			engine.setEnabled(false, gains);
		}
	}

	function schedulePersist(): void {
		if (typeof window === 'undefined') return;
		if (persistTimer) clearTimeout(persistTimer);
		persistTimer = setTimeout(() => {
			persistTimer = null;
			localStorage.setItem(STORAGE_KEY, JSON.stringify({ enabled, gains, activePreset }));
		}, PERSIST_DEBOUNCE_MS);
	}

	function detectPreset(): void {
		for (const [name, preset] of Object.entries(EQ_PRESETS)) {
			if (preset.every((v, i) => v === gains[i])) {
				activePreset = name;
				return;
			}
		}
		activePreset = null;
	}

	function toggleEq(): void {
		enabled = !enabled;
		syncToEngine();
		schedulePersist();
	}

	function setBandGain(index: number, dB: number): void {
		if (index < 0 || index >= EQ_BAND_COUNT) return;
		gains[index] = clampGain(dB);
		detectPreset();
		syncToEngine();
		schedulePersist();
	}

	function applyPreset(name: EqPresetName): void {
		const preset = EQ_PRESETS[name];
		if (!preset) return;
		gains = [...preset];
		activePreset = name;
		syncToEngine();
		schedulePersist();
	}

	function resetToFlat(): void {
		applyPreset('Flat');
	}

	function replayToEngine(): void {
		syncToEngine();
	}

	// Apply initial state to engine if already connected
	syncToEngine();

	return {
		get enabled() {
			return enabled;
		},
		get gains(): readonly number[] {
			return gains;
		},
		get activePreset() {
			return activePreset;
		},
		toggleEq,
		setBandGain,
		applyPreset,
		resetToFlat,
		replayToEngine
	};
}

export const eqStore = createEqStore();
