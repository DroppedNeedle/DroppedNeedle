// The site's CSS and a person's own, kept on their account and applied in
// the signed-in shell as two constructed stylesheets, the site's first. The
// policy's `img-src` allows any host and does not govern a constructed
// sheet, so the check below is what keeps this CSS from fetching. The
// server runs the same check on every save and is the one that counts; this
// copy runs before every apply and gives quick feedback while typing. Both
// read `customCssRules.json` and pass its cases.
import { browser } from '$app/environment';
import { untrack } from 'svelte';
import { SvelteURL } from 'svelte/reactivity';
import { ApiError } from '$lib/api/errors';
import {
	createImportAppearanceMutation,
	getMyAppearanceQuery,
	takeFetchedAppearance,
	type AppearanceUpdate,
	type CssProblem,
	type MyAppearance,
	type ThemeChoice
} from '$lib/features/profile/queries/appearance.svelte';
import { AppearanceQueryKeyFactory } from '$lib/queries/featureKeys';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import rules from '$lib/stores/customCssRules.json';
import { THEME_STORAGE_KEY, themeStore } from '$lib/stores/theme.svelte';
import { registerUserSessionReset } from '$lib/utils/userSessionCleanup';

/** Whose sheet is checked, which sets its length limit. */
export type CssLimit = keyof typeof rules.limits;

export const MAX_CUSTOM_CSS = rules.limits.user;
export const MAX_SITE_CSS = rules.limits.site;

const ALLOWED_FUNCTIONS: ReadonlySet<string> = new Set(rules.allowed_functions);

// The key the browser-only version kept a person's CSS under, read once to
// move it to their account.
const OLD_KEY_PREFIX = 'droppedneedle_custom_css:';
const THEMES: readonly ThemeChoice[] = ['system', 'light', 'dark', 'midnight'];

/** A name character the check reads before a `(`: ASCII only, as the server reads it. */
function isNameUnit(unit: number): boolean {
	return (
		(unit >= 0x30 && unit <= 0x39) ||
		(unit >= 0x41 && unit <= 0x5a) ||
		(unit >= 0x61 && unit <= 0x7a) ||
		unit === 0x5f ||
		unit === 0x2d
	);
}

/** Where each line starts. A line ends at `\n`, `\r\n`, `\r` or `\f`, as CSS reads them. */
function lineStarts(css: string): number[] {
	const starts = [0];
	for (let at = 0; at < css.length; at++) {
		const unit = css[at];
		if (unit === '\r' && css[at + 1] === '\n') {
			at++;
			starts.push(at + 1);
		} else if (unit === '\n' || unit === '\r' || unit === '\f') {
			starts.push(at + 1);
		}
	}
	return starts;
}

/**
 * What in `css` is refused, each once at the line it first appears on: a
 * backslash, `@import`, `@font-face`, then each function off the allowlist
 * in the order met. A sheet past its limit answers `too_long` alone. A
 * backslash is refused outright because an escape can spell any name
 * (`u\72l(` is `url(`). Every run of name characters before a `(` must be an
 * allowed function: a comment or space between a name and its `(` makes no
 * function in CSS, and a longer run than the real name is still not on the
 * list. Letters match in either case, ASCII only, as the server matches them.
 */
export function customCssProblems(css: string, limit: CssLimit = 'user'): CssProblem[] {
	if (css.length > rules.limits[limit]) return [{ what: 'too_long', line: 0 }];
	const starts = lineStarts(css);
	const lineOf = (at: number) => {
		let line = 0;
		while (line < starts.length && (starts[line] as number) <= at) line++;
		return line;
	};
	const lower = css.replace(/[A-Z]/g, (letter) => letter.toLowerCase());
	const found: CssProblem[] = [];
	const refuse = (what: string, at: number) => {
		if (at >= 0 && !found.some((problem) => problem.what === what)) {
			found.push({ what, line: lineOf(at) });
		}
	};
	refuse('backslash', css.indexOf('\\'));
	refuse('import', lower.indexOf('@import'));
	refuse('font_face', lower.indexOf('@font-face'));
	for (let at = lower.indexOf('('); at >= 0; at = lower.indexOf('(', at + 1)) {
		let start = at;
		while (start > 0 && isNameUnit(lower.charCodeAt(start - 1))) start--;
		if (start === at) continue;
		const name = lower.slice(start, at);
		if (!ALLOWED_FUNCTIONS.has(name)) refuse(`function:${name}`, start);
	}
	return found;
}

/** The problems as one short line for the screen, or null when there are none. */
export function describeCssProblems(
	problems: CssProblem[],
	limit: CssLimit = 'user'
): string | null {
	if (problems.length === 0) return null;
	if (problems.some((problem) => problem.what === 'too_long')) {
		return `Longer than ${rules.limits[limit].toLocaleString()} characters.`;
	}
	const words = problems.map(({ what }) => {
		if (what === 'backslash') return 'backslashes';
		if (what === 'import') return '@import';
		if (what === 'font_face') return '@font-face';
		return what.startsWith('function:') ? `${what.slice('function:'.length)}()` : what;
	});
	return `Not allowed: ${words.join(', ')}.`;
}

/** What in `css` is refused, as one short line, or null when it may be applied. */
export function customCssProblem(css: string, limit: CssLimit = 'user'): string | null {
	return describeCssProblems(customCssProblems(css, limit), limit);
}

function problemsIn(details: unknown): CssProblem[] {
	if (typeof details !== 'object' || details === null || !('problems' in details)) return [];
	const { problems } = details as { problems: unknown };
	return Array.isArray(problems) ? (problems as CssProblem[]) : [];
}

/** What a refused save of custom CSS answers, worded for the screen. */
export function customCssSaveError(error: unknown, limit: CssLimit = 'user'): string {
	if (error instanceof ApiError) {
		if (error.code === 'CUSTOM_CSS_REFUSED') {
			return describeCssProblems(problemsIn(error.details), limit) ?? 'This CSS is refused.';
		}
		if (error.code === 'CUSTOM_CSS_TOO_LONG') {
			return `Longer than ${rules.limits[limit].toLocaleString()} characters.`;
		}
		if (error.code === 'CUSTOM_CSS_TURNED_OFF') return 'Custom CSS is turned off on this server.';
	}
	return error instanceof Error && error.message ? error.message : 'Saving failed.';
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
// address an apply sees, keeps both sheets off until a full reload, so a
// sign-in redirect that drops the parameter does not bring them back.
function hatchAtLoad(): boolean {
	if (!browser) return false;
	const [entry] = performance.getEntriesByType('navigation');
	return hatchIn(entry?.name) || hatchIn(window.location.href);
}

function canAdopt(): boolean {
	return browser && typeof CSSStyleSheet !== 'undefined' && 'adoptedStyleSheets' in document;
}

function createCustomCssStore() {
	let owner = $state<string | null>(null);
	let css = $state('');
	let enabled = $state(true);
	let siteCss = $state('');
	let allowUserCss = $state(true);
	let disabled = $state(hatchAtLoad());
	let siteSheet: CSSStyleSheet | null = null;
	let ownSheet: CSSStyleSheet | null = null;

	/** Take `previous` off the page and put on the sheets that hold rules, site first. */
	function place(previous: (CSSStyleSheet | null)[]): void {
		const others = document.adoptedStyleSheets.filter((one) => !previous.includes(one));
		const ours = [siteSheet, ownSheet].filter((one): one is CSSStyleSheet => one !== null);
		document.adoptedStyleSheets = [...others, ...ours];
	}

	/** The sheet holding `text`, or null when there is nothing the check lets through. */
	function sheetFor(
		current: CSSStyleSheet | null,
		text: string,
		limit: CssLimit
	): CSSStyleSheet | null {
		if (!text.trim() || customCssProblems(text, limit).length > 0) return null;
		const sheet = current ?? new CSSStyleSheet();
		sheet.replaceSync(text);
		return sheet;
	}

	function apply(): void {
		if (!canAdopt()) return;
		if (!disabled && hatchIn(window.location.href)) disabled = true;
		const on = !disabled && owner !== null;
		const previous = [siteSheet, ownSheet];
		siteSheet = on ? sheetFor(siteSheet, siteCss, 'site') : null;
		ownSheet = on && enabled && allowUserCss ? sheetFor(ownSheet, css, 'user') : null;
		place(previous);
	}

	return {
		/** The user whose appearance is loaded, or null while nobody's is. */
		get owner() {
			return owner;
		},
		/** The loaded user's own CSS, applied or not. */
		get css() {
			return css;
		},
		/** The loaded user's "Use my CSS" switch. */
		get enabled() {
			return enabled;
		},
		/** Whether the site lets people's own CSS apply. */
		get allowUserCss() {
			return allowUserCss;
		},
		/** True while `?nocustomcss=1` holds both sheets off. */
		get disabled() {
			return disabled;
		},

		/** Apply `userId`'s appearance. A sheet the check refuses is not applied. */
		showFor(userId: string, appearance: MyAppearance): void {
			owner = userId;
			css = appearance.custom_css.text;
			enabled = appearance.custom_css.enabled;
			siteCss = appearance.site.css;
			allowUserCss = appearance.site.allow_user_css;
			apply();
		},

		/** Take both sheets off the page. */
		hide(): void {
			owner = null;
			css = '';
			enabled = true;
			siteCss = '';
			allowUserCss = true;
			apply();
		},

		/**
		 * Why `next` cannot be saved for the loaded user, or null when it can.
		 * Clearing it always can, even where this browser cannot apply a sheet.
		 */
		saveProblem(next: string): string | null {
			if (!owner) return 'Sign in to save custom CSS.';
			const problem = customCssProblem(next);
			if (problem) return problem;
			if (next.trim() && !canAdopt()) return 'This browser cannot apply custom CSS.';
			return null;
		}
	};
}

export const customCssStore = createCustomCssStore();

// Signing out takes the sheets off, so the sign-in page and the next person
// on this browser never see them. A clean-up that keeps the signed-in user
// (another tab switched accounts) leaves that user's sheets on.
registerUserSessionReset(() => {
	if (customCssStore.owner !== authStore.user?.id) customCssStore.hide();
});

function readOldCss(userId: string): string {
	try {
		return localStorage.getItem(`${OLD_KEY_PREFIX}${userId}`) ?? '';
	} catch {
		return '';
	}
}

function removeOldCss(userId: string): void {
	try {
		localStorage.removeItem(`${OLD_KEY_PREFIX}${userId}`);
	} catch {
		// Storage refused: the account holds the sheet, so a leftover copy is harmless.
	}
}

function readLocalTheme(): ThemeChoice | null {
	try {
		const raw = localStorage.getItem(THEME_STORAGE_KEY);
		return THEMES.find((theme) => theme === raw) ?? null;
	} catch {
		return null;
	}
}

function isRefusal(error: unknown): boolean {
	return error instanceof ApiError && (error.status === 400 || error.status === 403);
}

/**
 * The account and this browser's theme follow the signed-in user, and the
 * site's and their own CSS apply. The first time an account answers with
 * nothing stored, the theme and CSS this browser kept move up to it. Call
 * once from the signed-in shell's script.
 */
export function applyAppearance(): void {
	const query = getMyAppearanceQuery();
	const importer = createImportAppearanceMutation();
	const mountedAt = Date.now();
	// The users this page already tried to move, so a refusal is not retried on every answer.
	const moveTried: string[] = [];

	async function moveFromBrowser(userId: string): Promise<void> {
		const theme = readLocalTheme();
		const text = readOldCss(userId);
		if (!theme && !text.trim()) return;
		const update: AppearanceUpdate = {};
		if (theme) update.theme = theme;
		if (text.trim()) update.custom_css = { text };
		try {
			await importer.mutateAsync(update);
			removeOldCss(userId);
		} catch (error) {
			// A refused sheet stays in this browser; the theme still moves on its own.
			if (!isRefusal(error) || !theme || !update.custom_css) return;
			await importer.mutateAsync({ theme }).catch(() => undefined);
		}
	}

	$effect(() => {
		const userId = authStore.user?.id;
		if (!userId) return;
		// A real change of user takes the last one's sheets off.
		return () => customCssStore.hide();
	});

	$effect(() => {
		const userId = authStore.user?.id;
		// Read for their changes only: the answer is read below by the user's
		// own key, so a moment where the query still holds the last user's
		// answer never applies it.
		void query.data;
		void query.dataUpdatedAt;
		if (!userId) return;
		untrack(() => {
			const appearance = queryClient.getQueryData<MyAppearance>(
				AppearanceQueryKeyFactory.mine(userId)
			);
			if (!appearance) return;
			customCssStore.showFor(userId, appearance);
			if (appearance.theme) themeStore.set(appearance.theme);
			const fetched = takeFetchedAppearance();
			if (
				fetched &&
				fetched.userId === userId &&
				fetched.fetchedAt >= mountedAt &&
				!fetched.answer.stored &&
				!moveTried.includes(userId)
			) {
				moveTried.push(userId);
				void moveFromBrowser(userId);
			}
		});
	});
}
