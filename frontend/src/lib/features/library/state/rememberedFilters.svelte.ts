// A library list's filters, kept in the browser per list: the list writes
// its filter parameters back whenever they change, and a visit whose
// address carries none of them gets the kept ones. A link that names a
// filter wins over what was kept.
import { goto } from '$app/navigation';
import { page } from '$app/state';
import { SvelteURL } from 'svelte/reactivity';

const PREFIX = 'library.filters.';

function read(list: string): Record<string, string> {
	try {
		const kept = JSON.parse(localStorage.getItem(PREFIX + list) ?? '{}');
		return kept && typeof kept === 'object' ? kept : {};
	} catch {
		return {};
	}
}

/**
 * Keep `keys` of the address for `list`. Call once from the list's
 * component; it restores on the first visit and saves from then on.
 */
export function rememberFilters(list: string, keys: readonly string[]): void {
	let restored = false;
	$effect(() => {
		const params = page.url.searchParams;
		const current: Record<string, string> = {};
		for (const key of keys) {
			const value = params.get(key);
			if (value) current[key] = value;
		}
		if (!restored) {
			restored = true;
			const kept = read(list);
			if (Object.keys(current).length === 0 && Object.keys(kept).length > 0) {
				const url = new SvelteURL(page.url);
				for (const key of keys) if (kept[key]) url.searchParams.set(key, kept[key]);
				void goto(url, { replaceState: true, keepFocus: true, noScroll: true });
				return;
			}
		}
		if (Object.keys(current).length > 0) {
			localStorage.setItem(PREFIX + list, JSON.stringify(current));
		} else {
			localStorage.removeItem(PREFIX + list);
		}
	});
}
