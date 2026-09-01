import { browser } from '$app/environment';

interface CachedEntry<T> {
	data: T;
	timestamp: number;
}

interface LocalStorageCacheOptions {
	maxEntries?: number;
	/** Recomputed for every access so source/user changes cannot reuse old data. */
	keyNamespace?: () => string;
}

interface LocalStorageRecord<T> {
	key: string;
	entry: CachedEntry<T>;
}

export function createLocalStorageCache<T>(
	baseKey: string,
	initialTtl: number,
	options: LocalStorageCacheOptions = {}
) {
	let ttl = initialTtl;
	const keyPrefix = `${baseKey}_`;
	const maxEntries = options.maxEntries;
	const keyNamespace = options.keyNamespace;

	function scopedBaseKey(): string {
		return keyNamespace ? `${baseKey}_${keyNamespace()}` : baseKey;
	}

	function resolveKey(suffix?: string): string {
		const scopedBase = scopedBaseKey();
		return suffix ? `${scopedBase}_${suffix}` : scopedBase;
	}

	function parseEntry(raw: string | null): CachedEntry<T> | null {
		if (!raw) return null;
		try {
			const parsed = JSON.parse(raw) as CachedEntry<T>;
			if (typeof parsed?.timestamp !== 'number') {
				return null;
			}
			return parsed;
		} catch {
			return null;
		}
	}

	function getMatchingKeys(): string[] {
		const keys: string[] = [];
		for (let i = 0; i < localStorage.length; i++) {
			const key = localStorage.key(i);
			if (!key) continue;
			if (key === baseKey || key.startsWith(keyPrefix)) {
				keys.push(key);
			}
		}
		return keys;
	}

	function getRecords(): LocalStorageRecord<T>[] {
		const records: LocalStorageRecord<T>[] = [];
		for (const key of getMatchingKeys()) {
			const entry = parseEntry(localStorage.getItem(key));
			if (!entry) {
				localStorage.removeItem(key);
				continue;
			}
			records.push({ key, entry });
		}
		return records;
	}

	function isStale(timestamp: number): boolean {
		return Date.now() - timestamp > ttl;
	}

	function isSuffixKey(key: string): boolean {
		return key.startsWith(keyPrefix);
	}

	function _isQuotaExceededError(error: unknown): boolean {
		if (!(error instanceof DOMException)) return false;
		return (
			error.code === 22 ||
			error.code === 1014 ||
			error.name === 'QuotaExceededError' ||
			error.name === 'NS_ERROR_DOM_QUOTA_REACHED'
		);
	}

	function removeStaleEntries(): number {
		if (!browser) return 0;
		let removed = 0;
		for (const { key, entry } of getRecords()) {
			if (isStale(entry.timestamp)) {
				localStorage.removeItem(key);
				removed += 1;
			}
		}
		return removed;
	}

	function enforceMaxEntriesLimit(): number {
		if (!browser) return 0;
		if (!maxEntries || maxEntries <= 0) return 0;

		const suffixRecords = getRecords()
			.filter((record) => isSuffixKey(record.key))
			.sort((a, b) => b.entry.timestamp - a.entry.timestamp);

		if (suffixRecords.length <= maxEntries) return 0;

		let removed = 0;
		for (const record of suffixRecords.slice(maxEntries)) {
			localStorage.removeItem(record.key);
			removed += 1;
		}

		return removed;
	}

	function removeOldestEntry(): boolean {
		if (!browser) return false;
		const oldest = getRecords()
			.filter((record) => isSuffixKey(record.key))
			.sort((a, b) => a.entry.timestamp - b.entry.timestamp)[0];
		if (!oldest) return false;
		localStorage.removeItem(oldest.key);
		return true;
	}

	function tryWrite(key: string, payload: string): boolean {
		try {
			localStorage.setItem(key, payload);
			return true;
		} catch {
			return false;
		}
	}

	function get(suffix?: string): CachedEntry<T> | null {
		if (!browser) return null;
		const key = resolveKey(suffix);
		const raw = localStorage.getItem(key);
		const entry = parseEntry(raw);
		if (!entry && raw) {
			localStorage.removeItem(key);
		}
		if (!entry) {
			return null;
		}
		return entry;
	}

	function set(data: T, suffix?: string): void {
		if (!browser) return;
		const key = resolveKey(suffix);
		const payload = JSON.stringify({ data, timestamp: Date.now() } satisfies CachedEntry<T>);

		if (tryWrite(key, payload)) {
			enforceMaxEntriesLimit();
			return;
		}

		removeStaleEntries();
		enforceMaxEntriesLimit();

		if (tryWrite(key, payload)) {
			enforceMaxEntriesLimit();
			return;
		}

		if (removeOldestEntry() && tryWrite(key, payload)) {
			enforceMaxEntriesLimit();
			return;
		}
	}

	function remove(suffix?: string): void {
		if (!browser) return;
		localStorage.removeItem(resolveKey(suffix));
	}

	function updateTTL(newTtl: number): void {
		ttl = newTtl;
	}

	return { get, set, remove, isStale, updateTTL };
}

// Clears user-scoped caches on logout / user switch; the TanStack query-cache reset doesn't cover these.
export function clearLocalStorageNamespace(baseKey: string): void {
	if (!browser) return;
	const prefix = `${baseKey}_`;
	const toRemove: string[] = [];
	for (let i = 0; i < localStorage.length; i++) {
		const key = localStorage.key(i);
		if (!key) continue;
		if (key === baseKey || key.startsWith(prefix)) toRemove.push(key);
	}
	for (const key of toRemove) localStorage.removeItem(key);
}
