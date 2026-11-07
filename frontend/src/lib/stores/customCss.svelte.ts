// A person's own CSS, kept in this browser per user and applied in the
// signed-in shell as one constructed stylesheet. The policy's `img-src`
// allows any host and does not govern a constructed sheet, so the check
// below is the only thing keeping this CSS from fetching: it runs on the
// raw text at save and again before every apply.
import { browser } from '$app/environment';
import { untrack } from 'svelte';
import { SvelteURL } from 'svelte/reactivity';
import { authStore } from '$lib/stores/authStore.svelte';
import { registerUserSessionReset } from '$lib/utils/userSessionCleanup';

export const MAX_CUSTOM_CSS = 20_000;
const KEY_PREFIX = 'droppedneedle_custom_css:';

// The CSS functions a sheet may call. An allowlist, so a fetching function
// (url, image-set, src, cross-fade, element, paint) and any a browser adds
// later are refused.
const ALLOWED_FUNCTIONS = new Set([
	// values and maths
	'var',
	'env',
	'calc',
	'min',
	'max',
	'clamp',
	'round',
	'mod',
	'rem',
	'abs',
	'sign',
	'sin',
	'cos',
	'tan',
	'asin',
	'acos',
	'atan',
	'atan2',
	// colours
	'rgb',
	'rgba',
	'hsl',
	'hsla',
	'hwb',
	'lab',
	'lch',
	'oklab',
	'oklch',
	'color',
	'color-mix',
	'light-dark',
	// gradients
	'linear-gradient',
	'radial-gradient',
	'conic-gradient',
	'repeating-linear-gradient',
	'repeating-radial-gradient',
	'repeating-conic-gradient',
	// transforms
	'translate',
	'translatex',
	'translatey',
	'translatez',
	'translate3d',
	'scale',
	'scalex',
	'scaley',
	'scalez',
	'scale3d',
	'rotate',
	'rotatex',
	'rotatey',
	'rotatez',
	'rotate3d',
	'skew',
	'skewx',
	'skewy',
	'matrix',
	'matrix3d',
	'perspective',
	// filters
	'blur',
	'brightness',
	'contrast',
	'drop-shadow',
	'grayscale',
	'hue-rotate',
	'invert',
	'opacity',
	'saturate',
	'sepia',
	// shapes, timing, grid and generated content
	'inset',
	'circle',
	'ellipse',
	'polygon',
	'cubic-bezier',
	'steps',
	'linear',
	'repeat',
	'minmax',
	'fit-content',
	'counter',
	'counters',
	'attr',
	// selectors
	'is',
	'where',
	'not',
	'has',
	'nth-child',
	'nth-last-child',
	'nth-of-type',
	'nth-last-of-type',
	'lang',
	'dir'
]);

/**
 * What in `css` is refused, as one short line, or null when it may be
 * applied. A backslash is refused outright because an escape can spell any
 * name (`u\72l(` is `url(`). Every run of name characters before a `(` must
 * be an allowed function: a comment or space between a name and its `(`
 * makes no function in CSS, and a longer run than the real name is still
 * not on the list.
 */
export function customCssProblem(css: string): string | null {
	if (css.length > MAX_CUSTOM_CSS) {
		return `Longer than ${MAX_CUSTOM_CSS.toLocaleString()} characters.`;
	}
	const refused: string[] = [];
	const refuse = (what: string) => {
		if (!refused.includes(what)) refused.push(what);
	};
	if (css.includes('\\')) refuse('backslashes');
	if (/@import/i.test(css)) refuse('@import');
	if (/@font-face/i.test(css)) refuse('@font-face');
	for (const [, name] of css.matchAll(/([a-z0-9_-]+)\(/gi)) {
		const lower = name.toLowerCase();
		if (!ALLOWED_FUNCTIONS.has(lower)) refuse(`${lower}()`);
	}
	return refused.length > 0 ? `Not allowed: ${refused.join(', ')}.` : null;
}

function hatchIn(address: string | undefined): boolean {
	if (!address) return false;
	try {
		return new SvelteURL(address, 'http://localhost').searchParams.get('nocustomcss') === '1';
	} catch {
		return false;
	}
}

// `?nocustomcss=1` on the address the page was loaded from, or on any
// address an apply sees, keeps the sheet off until a full reload, so a
// sign-in redirect that drops the parameter does not bring it back.
function hatchAtLoad(): boolean {
	if (!browser) return false;
	const [entry] = performance.getEntriesByType('navigation');
	return hatchIn(entry?.name) || hatchIn(window.location.href);
}

function storageKey(userId: string): string {
	return `${KEY_PREFIX}${userId}`;
}

function readStored(userId: string): string {
	try {
		return localStorage.getItem(storageKey(userId)) ?? '';
	} catch {
		return '';
	}
}

function createCustomCssStore() {
	let owner = $state<string | null>(null);
	let css = $state('');
	let disabled = $state(hatchAtLoad());
	let sheet: CSSStyleSheet | null = null;

	function removeSheet(): void {
		if (!sheet) return;
		const applied = sheet;
		document.adoptedStyleSheets = document.adoptedStyleSheets.filter((one) => one !== applied);
		sheet = null;
	}

	function apply(): void {
		if (!browser) return;
		if (!disabled && hatchIn(window.location.href)) disabled = true;
		if (disabled || !css.trim() || customCssProblem(css) !== null) {
			removeSheet();
			return;
		}
		if (!sheet) {
			sheet = new CSSStyleSheet();
			document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
		}
		sheet.replaceSync(css);
	}

	return {
		/** The user whose CSS is loaded, or null while nobody's is. */
		get owner() {
			return owner;
		},
		get css() {
			return css;
		},
		/** True while `?nocustomcss=1` holds the sheet off. */
		get disabled() {
			return disabled;
		},

		/** Load and apply `userId`'s stored CSS. A stored sheet the check refuses is not applied. */
		showFor(userId: string): void {
			owner = userId;
			css = readStored(userId);
			apply();
		},

		/** Take the sheet off the page. The stored CSS stays for its user. */
		hide(): void {
			owner = null;
			css = '';
			removeSheet();
		},

		/** Save and apply `next` for the loaded user, or answer what the check refused. */
		save(next: string): string | null {
			if (!owner) return null;
			const problem = customCssProblem(next);
			if (problem) return problem;
			if (typeof CSSStyleSheet === 'undefined' || !('adoptedStyleSheets' in document)) {
				return 'This browser cannot apply custom CSS.';
			}
			try {
				if (next.trim()) localStorage.setItem(storageKey(owner), next);
				else localStorage.removeItem(storageKey(owner));
			} catch {
				// Storage refused (private window, quota): it lasts for this page only.
			}
			css = next;
			apply();
			return null;
		}
	};
}

export const customCssStore = createCustomCssStore();

// Signing out takes the sheet off, so the sign-in page and the next person
// on this browser never see it.
registerUserSessionReset(() => customCssStore.hide());

/**
 * Apply the signed-in user's CSS and follow account changes. Call once from
 * the signed-in shell's script.
 */
export function applyCustomCss(): void {
	$effect(() => {
		const userId = authStore.user?.id;
		if (!userId) return;
		untrack(() => customCssStore.showFor(userId));
		return () => customCssStore.hide();
	});
}
