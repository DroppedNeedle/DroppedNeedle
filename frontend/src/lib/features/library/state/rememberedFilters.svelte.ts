// A library list's filters, kept in the browser per user and list: the
// list writes its filter parameters back whenever they change, and a visit
// whose address carries none of them gets the kept ones. A link that names
// a filter wins over what was kept. Signing out forgets them all.
import { goto } from '$app/navigation';
import { page } from '$app/state';
import { SvelteURL } from 'svelte/reactivity';
import { authStore } from '$lib/stores/authStore.svelte';
import { registerUserSessionReset } from '$lib/utils/userSessionCleanup';

const PREFIX = 'library.filters.';

function storageKey(list: string): string | null {
	const userId = authStore.user?.id;
	return userId ? `${PREFIX}${userId}.${list}` : null;
}

function read(key: string): Record<string, string> {
	try {
		const kept = JSON.parse(localStorage.getItem(key) ?? '{}');
		return kept && typeof kept === 'object' ? kept : {};
	} catch {
		return {};
	}
}

function write(key: string, current: Record<string, string>): void {
	try {
		if (Object.keys(current).length > 0) localStorage.setItem(key, JSON.stringify(current));
		else localStorage.removeItem(key);
	} catch {
		// Storage refused (private window, quota): the filters just aren't kept.
	}
}

/**
 * One kept value of `list`, read now. For a list whose default must be in place
 * before its first render, which a restored address cannot do.
 */
export function keptFilter(list: string, name: string): string | null {
	const key = storageKey(list);
	if (!key) return null;
	return read(key)[name] ?? null;
}

/** Keep one value of `list` beside its filters; `null` forgets it. */
export function keepFilter(list: string, name: string, value: string | null): void {
	const key = storageKey(list);
	if (!key) return;
	const kept = read(key);
	if (value === null) delete kept[name];
	else kept[name] = value;
	write(key, kept);
}

// A shared browser never carries one person's filters to the next.
registerUserSessionReset(() => {
	try {
		for (let index = localStorage.length - 1; index >= 0; index--) {
			const key = localStorage.key(index);
			if (key?.startsWith(PREFIX)) localStorage.removeItem(key);
		}
	} catch {
		// Storage unreadable: nothing was kept.
	}
});

/**
 * Keep `keys` of the address for `list`. Call once from the list's
 * component; it restores on the first visit and saves from then on.
 */
export function rememberFilters(list: string, keys: readonly string[]): void {
	let restored = false;
	$effect(() => {
		const key = storageKey(list);
		if (!key) return;
		const params = page.url.searchParams;
		const current: Record<string, string> = {};
		for (const name of keys) {
			const value = params.get(name);
			if (value) current[name] = value;
		}
		if (!restored) {
			restored = true;
			const kept = read(key);
			if (Object.keys(current).length === 0 && Object.keys(kept).length > 0) {
				const url = new SvelteURL(page.url);
				for (const name of keys) if (kept[name]) url.searchParams.set(name, kept[name]);
				void goto(url, { replaceState: true, keepFocus: true, noScroll: true });
				return;
			}
		}
		write(key, current);
	});
}
