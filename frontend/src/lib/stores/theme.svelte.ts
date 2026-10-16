import { browser } from '$app/environment';

export type ThemePreference = 'system' | 'light' | 'dark';
type Theme = 'light' | 'dark';

const STORAGE_KEY = 'droppedneedle_theme';
const LIGHT_QUERY = '(prefers-color-scheme: light)';
// Browser chrome colour per theme: each theme's base-100 in src/theme.css.
// Hardcoded because the theme is applied before any stylesheet has loaded.
const THEME_COLOR: Record<Theme, string> = { dark: '#0d120a', light: '#f6f8f3' };

export function parseThemePreference(raw: string | null): ThemePreference {
	return raw === 'system' || raw === 'light' ? raw : 'dark';
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

function apply(preference: ThemePreference): void {
	const theme = resolveTheme(preference, matchMedia(LIGHT_QUERY).matches);
	document.documentElement.dataset.theme = theme;
	document.querySelector('meta[name="theme-color"]')?.setAttribute('content', THEME_COLOR[theme]);
}

function createThemeStore() {
	const initial = readPreference();
	let preference = $state(initial);

	// The root layout imports this module, and the app's stylesheets only
	// arrive with the lazily loaded shell, so the stored theme is in place
	// before anything themed paints.
	if (browser) {
		apply(initial);
		matchMedia(LIGHT_QUERY).addEventListener('change', () => {
			if (preference === 'system') apply(preference);
		});
	}

	function set(next: ThemePreference): void {
		preference = next;
		try {
			localStorage.setItem(STORAGE_KEY, next);
		} catch {
			// private mode: the choice lasts for this page only
		}
		apply(next);
	}

	return {
		get preference() {
			return preference;
		},
		set
	};
}

export const themeStore = createThemeStore();
