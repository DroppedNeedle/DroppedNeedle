import { browser } from '$app/environment';
import { SvelteURLSearchParams } from 'svelte/reactivity';

export const MAX_CUSTOM_CSS = 20_000;
const STORAGE_KEY = 'droppedneedle_custom_css';

export function isCustomCssDisabled(search: string): boolean {
	return new SvelteURLSearchParams(search).get('nocustomcss') === '1';
}

export function clampCustomCss(css: string): string {
	return css.slice(0, MAX_CUSTOM_CSS);
}

function readCss(): string {
	if (!browser) return '';
	try {
		return clampCustomCss(localStorage.getItem(STORAGE_KEY) ?? '');
	} catch {
		return '';
	}
}

function createCustomCssStore() {
	const initial = readCss();
	let css = $state(initial);
	const disabled = browser && isCustomCssDisabled(window.location.search);
	let sheet: CSSStyleSheet | null = null;

	function removeSheet(): void {
		if (!sheet) return;
		document.adoptedStyleSheets = document.adoptedStyleSheets.filter(
			(candidate) => candidate !== sheet
		);
		sheet = null;
	}

	function apply(next: string): void {
		if (!browser || disabled) return;
		if (!next.trim()) {
			removeSheet();
			return;
		}
		if (typeof CSSStyleSheet === 'undefined' || !document.adoptedStyleSheets) return;
		if (!sheet) {
			sheet = new CSSStyleSheet();
			document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
		}
		try {
			sheet.replaceSync(next);
		} catch {
			// A sheet the parser refused is removed rather than left with the
			// previous rules.
			removeSheet();
		}
	}

	if (browser) apply(initial);

	function set(next: string): void {
		const value = clampCustomCss(next);
		css = value;
		try {
			localStorage.setItem(STORAGE_KEY, value);
		} catch {
			// private mode: the choice lasts for this page only
		}
		apply(value);
	}

	function clear(): void {
		css = '';
		try {
			localStorage.removeItem(STORAGE_KEY);
		} catch {
			// private mode
		}
		removeSheet();
	}

	return {
		get css() {
			return css;
		},
		get disabled() {
			return disabled;
		},
		set,
		clear
	};
}

export const customCssStore = createCustomCssStore();
