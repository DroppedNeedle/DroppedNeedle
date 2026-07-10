import { browser } from '$app/environment';

/*
 * Appearance store — owns the user's theme, text size, and motion preference.
 *
 * It drives three hooks on <html>:
 *   - data-theme        → the active DaisyUI theme (see app.css)
 *   - data-dn-text      → large-text scale (see tokens.css)
 *   - data-dn-motion    → explicit reduced-motion opt-in (see tokens.css)
 *
 * The first paint is handled by a tiny inline script in app.html (anti-FOUC);
 * this store keeps everything in sync afterwards and persists choices to
 * localStorage. Playback, data, and every other system are untouched — this is
 * presentation only.
 */

export type ThemeChoice =
	| 'system'
	| 'droppedneedle'
	| 'droppedneedle-light'
	| 'droppedneedle-contrast';
export type TextScale = 'sm' | 'md' | 'lg' | 'xl';
export type MotionChoice = 'system' | 'reduced';

export const THEME_OPTIONS: { value: ThemeChoice; label: string; hint: string }[] = [
	{ value: 'system', label: 'System', hint: 'Follow your device' },
	{ value: 'droppedneedle', label: 'Dark', hint: 'The signature look' },
	{ value: 'droppedneedle-light', label: 'Daylight', hint: 'Warm and bright' },
	{ value: 'droppedneedle-contrast', label: 'High contrast', hint: 'Maximum legibility' }
];

export const TEXT_SCALE_OPTIONS: { value: TextScale; label: string }[] = [
	{ value: 'sm', label: 'Compact' },
	{ value: 'md', label: 'Default' },
	{ value: 'lg', label: 'Large' },
	{ value: 'xl', label: 'Larger' }
];

const LS_KEY = 'dn-appearance';
const DARK_THEME = 'droppedneedle';
const LIGHT_THEME = 'droppedneedle-light';

/** the four configurable mobile bottom-nav slots (the 5th, Menu, is fixed) */
export const BOTTOM_NAV_DEFAULT: string[] = ['home', 'discovery', 'search', 'collection'];

let theme = $state<ThemeChoice>('system');
let textScale = $state<TextScale>('md');
let motion = $state<MotionChoice>('system');
let bottomNav = $state<string[]>([...BOTTOM_NAV_DEFAULT]);
let systemDark = $state(true);

let initialized = false;

function resolveTheme(): string {
	if (theme === 'system') return systemDark ? DARK_THEME : LIGHT_THEME;
	return theme;
}

function persist(): void {
	if (!browser) return;
	try {
		localStorage.setItem(LS_KEY, JSON.stringify({ theme, textScale, motion, bottomNav }));
	} catch {
		// storage may be unavailable (private mode); preferences just won't persist
	}
}

function apply(): void {
	if (!browser) return;
	const el = document.documentElement;
	el.setAttribute('data-theme', resolveTheme());

	if (textScale === 'md') el.removeAttribute('data-dn-text');
	else el.setAttribute('data-dn-text', textScale);

	// 'system' defers to the OS-level media queries already in the stylesheet;
	// only an explicit choice writes the hard override attribute.
	if (motion === 'reduced') el.setAttribute('data-dn-motion', 'reduced');
	else el.removeAttribute('data-dn-motion');

	const meta = document.querySelector('meta[name="theme-color"]');
	if (meta) {
		const bg = getComputedStyle(el).getPropertyValue('--color-base-100').trim();
		if (bg) meta.setAttribute('content', bg);
	}
}

export const appearance = {
	get theme() {
		return theme;
	},
	get textScale() {
		return textScale;
	},
	get motion() {
		return motion;
	},
	get resolvedTheme() {
		return resolveTheme();
	},
	get bottomNav() {
		return bottomNav;
	},
	get isDark() {
		return resolveTheme() !== LIGHT_THEME;
	},

	init(): void {
		if (!browser || initialized) return;
		initialized = true;

		try {
			const raw = localStorage.getItem(LS_KEY);
			if (raw) {
				const p = JSON.parse(raw) as Partial<{
					theme: ThemeChoice;
					textScale: TextScale;
					motion: MotionChoice;
					bottomNav: string[];
				}>;
				if (p.theme) theme = p.theme;
				if (p.textScale) textScale = p.textScale;
				if (p.motion) motion = p.motion;
				if (
					Array.isArray(p.bottomNav) &&
					p.bottomNav.length === BOTTOM_NAV_DEFAULT.length &&
					p.bottomNav.every((id) => typeof id === 'string')
				) {
					bottomNav = [...p.bottomNav];
				}
			}
		} catch {
			// malformed value; fall back to defaults
		}

		const mq = window.matchMedia('(prefers-color-scheme: dark)');
		systemDark = mq.matches;
		mq.addEventListener('change', (e) => {
			systemDark = e.matches;
			if (theme === 'system') apply();
		});

		apply();
	},

	setTheme(next: ThemeChoice): void {
		theme = next;
		persist();
		apply();
	},

	setTextScale(next: TextScale): void {
		textScale = next;
		persist();
		apply();
	},

	setMotion(next: MotionChoice): void {
		motion = next;
		persist();
		apply();
	},

	/** replace one bottom-nav slot; rejects duplicates by swapping the clash */
	setBottomNavSlot(index: number, id: string): void {
		if (index < 0 || index >= bottomNav.length) return;
		const next = [...bottomNav];
		const clash = next.indexOf(id);
		if (clash >= 0 && clash !== index) next[clash] = next[index];
		next[index] = id;
		bottomNav = next;
		persist();
	},

	resetBottomNav(): void {
		bottomNav = [...BOTTOM_NAV_DEFAULT];
		persist();
	}
};
