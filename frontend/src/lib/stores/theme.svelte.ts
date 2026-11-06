import { browser } from '$app/environment';

export type ThemePreference = 'system' | 'light' | 'dark' | 'midnight';
type Theme = 'light' | 'dark' | 'midnight';

const STORAGE_KEY = 'droppedneedle_theme';
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
		return parseThemePreference(localStorage.getItem(STORAGE_KEY));
	} catch {
		return 'dark';
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

	function set(next: ThemePreference): void {
		preference = next;
		try {
			localStorage.setItem(STORAGE_KEY, next);
		} catch {
			// private mode: the choice lasts for this page only
		}
		resolved = apply(next);
	}

	return {
		get preference() {
			return preference;
		},
		get resolved() {
			return resolved;
		},
		set
	};
}

export const themeStore = createThemeStore();
