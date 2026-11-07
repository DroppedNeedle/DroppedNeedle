import { browser } from '$app/environment';

export type ThemePreference = 'system' | 'light' | 'dark' | 'midnight';
type Theme = 'light' | 'dark' | 'midnight';

// This browser's copy, which paints the first frame. Once signed in, the
// account's theme is written here.
export const THEME_STORAGE_KEY = 'droppedneedle_theme';
// The owner tag (see appearance.svelte.ts) of the account this copy was last
// applied or chosen for. Another account's copy is never uploaded, and an
// account with no theme of its own sees the default.
export const THEME_OWNER_STORAGE_KEY = 'droppedneedle_theme_owner';
const LIGHT_QUERY = '(prefers-color-scheme: light)';
// Browser chrome colour per theme: each theme's base-100 in src/theme.css.
// Hardcoded because the theme is applied before any stylesheet has loaded.
const THEME_COLOR: Record<Theme, string> = {
	dark: '#090e0b',
	light: '#f6f3e8',
	midnight: '#090e0b'
};

export function parseThemePreference(raw: string | null): ThemePreference {
	return raw === 'system' || raw === 'light' || raw === 'midnight' ? raw : 'dark';
}

export function resolveTheme(preference: ThemePreference, prefersLight: boolean): Theme {
	if (preference === 'system') return prefersLight ? 'light' : 'dark';
	return preference;
}

function readPreference(): ThemePreference {
	if (!browser) return 'dark';
	try {
		return parseThemePreference(localStorage.getItem(THEME_STORAGE_KEY));
	} catch {
		return 'dark';
	}
}

/** The owner tag of this browser's theme copy, or null when no account has claimed it. */
export function themeOwner(): string | null {
	try {
		return localStorage.getItem(THEME_OWNER_STORAGE_KEY);
	} catch {
		return null;
	}
}

/** Record `tag` as the owner tag of this browser's theme copy. */
export function setThemeOwner(tag: string): void {
	try {
		localStorage.setItem(THEME_OWNER_STORAGE_KEY, tag);
	} catch {
		// private mode: nothing is kept, so there is nothing to own
	}
}

function apply(preference: ThemePreference): Theme {
	const theme = resolveTheme(preference, matchMedia(LIGHT_QUERY).matches);
	document.documentElement.dataset.theme = theme;
	document.querySelector('meta[name="theme-color"]')?.setAttribute('content', THEME_COLOR[theme]);
	return theme;
}

function createThemeStore() {
	const initial = readPreference();
	let preference = $state(initial);
	// The theme showing now: the preference with 'system' resolved
	let resolved = $state<Theme>('dark');

	// The root layout imports this module, and the app's stylesheets only
	// arrive with the lazily loaded shell, so the stored theme is in place
	// before anything themed paints.
	if (browser) {
		resolved = apply(initial);
		matchMedia(LIGHT_QUERY).addEventListener('change', () => {
			if (preference === 'system') resolved = apply(preference);
		});
	}

	/** Show `next` without keeping it as this browser's copy. */
	function show(next: ThemePreference): void {
		preference = next;
		resolved = apply(next);
	}

	/** Show `next` and keep it as this browser's copy. Saving it to the account is the caller's. */
	function set(next: ThemePreference): void {
		show(next);
		try {
			localStorage.setItem(THEME_STORAGE_KEY, next);
		} catch {
			// private mode: the choice lasts for this page only
		}
	}

	return {
		get preference() {
			return preference;
		},
		get resolved() {
			return resolved;
		},
		/** The theme a paper sheet switches to inside: Midnight's dark stock, else light. */
		get paper(): 'light' | 'midnight' {
			return resolved === 'midnight' ? 'midnight' : 'light';
		},
		set,
		show
	};
}

export const themeStore = createThemeStore();
