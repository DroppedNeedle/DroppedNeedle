import { browser } from '$app/environment';
import { SvelteSet } from 'svelte/reactivity';
import { authStore } from '$lib/stores/authStore.svelte';
import type { MusicBrainzSettingsResponse, MusicBrainzSourceMode } from './types';

const STORAGE_PREFIX = 'droppedneedle:musicbrainz-source:';

export interface MusicBrainzSourceScope {
	userId: string | null;
	sourceMode: MusicBrainzSourceMode;
	sourceId: string;
	generation: number;
}

const EMPTY_SCOPE: MusicBrainzSourceScope = {
	userId: null,
	sourceMode: 'brainzmash',
	sourceId: '',
	generation: 0
};

let currentScope = $state<MusicBrainzSourceScope>(EMPTY_SCOPE);

function storageKey(userId: string): string {
	return `${STORAGE_PREFIX}${encodeURIComponent(userId)}`;
}

function isSourceMode(value: unknown): value is MusicBrainzSourceMode {
	return (
		value === 'brainzmash' || value === 'official' || value === 'mirror' || value === 'community'
	);
}

type PersistedSourceScope = {
	sourceMode: MusicBrainzSourceMode;
	sourceId: string;
	generation: number;
};

type MusicBrainzSourceScopeListener = (
	next: MusicBrainzSourceScope,
	previous: MusicBrainzSourceScope
) => void;

const sourceScopeListeners = new SvelteSet<MusicBrainzSourceScopeListener>();

function isPersistedScope(value: unknown): value is PersistedSourceScope {
	if (typeof value !== 'object' || value === null) return false;
	const candidate = value as Record<string, unknown>;
	if (!('sourceMode' in candidate) || !('sourceId' in candidate) || !('generation' in candidate)) {
		return false;
	}
	return (
		isSourceMode(candidate.sourceMode) &&
		typeof candidate.sourceId === 'string' &&
		typeof candidate.generation === 'number' &&
		Number.isInteger(candidate.generation) &&
		candidate.generation >= 0
	);
}

function parsePersistedScope(userId: string, raw: string | null): MusicBrainzSourceScope | null {
	if (!raw) return null;
	try {
		const parsed: unknown = JSON.parse(raw);
		if (!isPersistedScope(parsed)) return null;
		return {
			userId,
			sourceMode: parsed.sourceMode,
			sourceId: parsed.sourceId,
			generation: parsed.generation
		};
	} catch {
		return null;
	}
}

function readPersistedScope(userId: string): MusicBrainzSourceScope | null | undefined {
	if (!browser) return null;
	try {
		return parsePersistedScope(userId, localStorage.getItem(storageKey(userId)));
	} catch {
		return undefined;
	}
}

function sameScope(left: MusicBrainzSourceScope, right: MusicBrainzSourceScope): boolean {
	return (
		left.userId === right.userId &&
		left.sourceMode === right.sourceMode &&
		left.sourceId === right.sourceId &&
		left.generation === right.generation
	);
}

function updateCurrentScope(next: MusicBrainzSourceScope): void {
	if (sameScope(currentScope, next)) return;
	const previous = { ...currentScope };
	currentScope = { ...next };
	for (const listener of sourceScopeListeners) {
		try {
			listener({ ...currentScope }, previous);
		} catch {
			// A cache listener must not prevent source identity updates.
		}
	}
}

function scopeForUser(userId: string | null): MusicBrainzSourceScope {
	if (!userId) return { ...EMPTY_SCOPE };
	const persisted = readPersistedScope(userId);
	return persisted ?? { ...EMPTY_SCOPE, userId };
}

export function subscribeMusicBrainzSourceScope(
	listener: MusicBrainzSourceScopeListener
): () => void {
	sourceScopeListeners.add(listener);
	return () => sourceScopeListeners.delete(listener);
}

export function watchMusicBrainzSourceScope(): () => void {
	if (!browser || typeof window === 'undefined') return () => {};
	const handleStorage = (event: StorageEvent) => {
		const userId = authStore.user?.id ?? null;
		if (!userId || (event.key !== null && event.key !== storageKey(userId))) return;
		if (event.key === null || event.newValue === null) {
			updateCurrentScope({ ...EMPTY_SCOPE, userId });
			return;
		}
		const persisted = parsePersistedScope(userId, event.newValue);
		if (persisted) updateCurrentScope(persisted);
	};
	window.addEventListener('storage', handleStorage);
	return () => window.removeEventListener('storage', handleStorage);
}

export function getMusicBrainzSourceScope(): MusicBrainzSourceScope {
	const userId = authStore.user?.id ?? null;
	return currentScope.userId === userId ? { ...currentScope } : { ...scopeForUser(userId) };
}
export function setMusicBrainzSourceScope(
	settings: Pick<MusicBrainzSettingsResponse, 'source_mode' | 'source_id' | 'generation'>,
	userId = authStore.user?.id ?? null
): void {
	const normalizedUserId = userId ?? null;
	const nextScope: MusicBrainzSourceScope = {
		userId: normalizedUserId,
		sourceMode: settings.source_mode,
		sourceId: settings.source_id,
		generation: settings.generation
	};
	updateCurrentScope(nextScope);
	if (browser && normalizedUserId) {
		try {
			localStorage.setItem(
				storageKey(normalizedUserId),
				JSON.stringify({
					sourceMode: settings.source_mode,
					sourceId: settings.source_id,
					generation: settings.generation
				})
			);
		} catch {
			// Browser persistence is best effort; keep the in-memory scope authoritative.
		}
	}
}

export function resetMusicBrainzSourceScope(): void {
	updateCurrentScope({ ...EMPTY_SCOPE });
}

export function musicBrainzSourceKey(userId?: string | null): {
	user_id: string | null;
	source_mode: MusicBrainzSourceMode;
	source_id: string;
	generation: number;
} {
	if (userId === undefined) {
		return { ...musicBrainzSourceKeyForScope(getMusicBrainzSourceScope()) };
	}
	const normalizedUserId = userId ?? null;
	const activeUserId = authStore.user?.id ?? null;
	const scope =
		normalizedUserId === activeUserId
			? getMusicBrainzSourceScope()
			: scopeForUser(normalizedUserId);
	return { ...musicBrainzSourceKeyForScope(scope) };
}
function musicBrainzSourceKeyForScope(scope: MusicBrainzSourceScope) {
	return {
		user_id: scope.userId,
		source_mode: scope.sourceMode,
		source_id: scope.sourceId,
		generation: scope.generation
	};
}
