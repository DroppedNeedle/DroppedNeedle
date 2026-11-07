// The site's CSS and a person's own, applied in the signed-in shell as two
// constructed stylesheets, the site's first. What they hold comes from the
// account (`features/profile/state/appearance.svelte.ts`). The
// policy's `img-src` allows any host and does not govern a constructed
// sheet, so the check below is what keeps this CSS from fetching. The
// server runs the same check on every save and is the one that counts; this
// copy runs before every apply and gives quick feedback while typing. Both
// read `customCssRules.json` and pass its cases.
import { browser } from '$app/environment';
import { SvelteURL } from 'svelte/reactivity';
import { ApiError } from '$lib/api/errors';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import rules from '$lib/stores/customCssRules.json';
import { registerUserSessionReset } from '$lib/utils/userSessionCleanup';

type CssProblem = components['schemas']['CssProblem'];
type MyAppearance = components['schemas']['MyAppearance'];

/** Whose sheet is checked, which sets its length limit. */
export type CssLimit = keyof typeof rules.limits;

export const MAX_CUSTOM_CSS = rules.limits.user;
export const MAX_SITE_CSS = rules.limits.site;

const ALLOWED_FUNCTIONS: ReadonlySet<string> = new Set(rules.allowed_functions);

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
	let draft = $state<string | null>(null);
	let previewing = $state(false);
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
		ownSheet = on && enabled && allowUserCss ? sheetFor(ownSheet, draft ?? css, 'user') : null;
		previewing = draft !== null && ownSheet !== null;
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
		/** True while an unsaved draft is applied in place of the saved sheet. */
		get previewing() {
			return previewing;
		},

		/** Apply `userId`'s appearance. A sheet the check refuses is not applied. */
		showFor(userId: string, appearance: MyAppearance): void {
			if (userId !== owner) draft = null;
			owner = userId;
			css = appearance.custom_css.text;
			enabled = appearance.custom_css.enabled;
			siteCss = appearance.site.css;
			allowUserCss = appearance.site.allow_user_css;
			apply();
		},

		/** Take both sheets off the page. */
		hide(): void {
			draft = null;
			owner = null;
			css = '';
			enabled = true;
			siteCss = '';
			allowUserCss = true;
			apply();
		},

		/**
		 * Apply `text` as the loaded user's sheet in place of the saved one,
		 * under the same check and switches; null puts the saved one back. A
		 * sign-out or a change of user drops the draft.
		 */
		preview(text: string | null): void {
			if (text !== null && !owner) return;
			draft = text;
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
