// The browser's copy of the custom CSS check answers every case the server's
// check passes. The site's sheet loads first and the person's on top, the
// person's only while the site lets it, and `?nocustomcss=1` holds both off.
import { afterEach, describe, expect, it } from 'vitest';
import type { components } from '$lib/api/v3/openapi';
import {
	customCssProblem,
	customCssProblems,
	customCssStore,
	type CssLimit
} from '$lib/stores/customCss.svelte';
import rules from '$lib/stores/customCssRules.json';

type MyAppearance = components['schemas']['MyAppearance'];

function token(name: string): string {
	return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

function appearance(own: string, site: string, allowUserCss = true): MyAppearance {
	return {
		theme: null,
		custom_css: { text: own, enabled: true, saved_at: 1 },
		site: { css: site, allow_user_css: allowUserCss },
		stored: true
	};
}

afterEach(() => customCssStore.hide());

describe('custom CSS rules', () => {
	it('answers every shared case as the server does', () => {
		const wrong = rules.cases.flatMap((rule) => {
			const css = rule.css.repeat('repeat' in rule ? (rule.repeat as number) : 1);
			const found = customCssProblems(css, rule.limit as CssLimit);
			return JSON.stringify(found) === JSON.stringify(rule.problems)
				? []
				: [`${rule.css.slice(0, 60)}: got ${JSON.stringify(found)}`];
		});
		expect(wrong).toEqual([]);
	});

	it('words each refusal for the screen', () => {
		expect(customCssProblem('a { b: url(x); }')).toBe('Not allowed: url().');
		expect(customCssProblem('@font-face {} @import "x"; a\\b {}')).toBe(
			'Not allowed: backslashes, @import, @font-face.'
		);
		expect(customCssProblem('a'.repeat(20_001))).toBe(
			`Longer than ${(20_000).toLocaleString()} characters.`
		);
	});
});

describe('custom CSS sheets', () => {
	it('loads the site sheet first and the person’s on top', () => {
		const site = ':root { --dn-spec-order: 1px; --dn-spec-site: 1px; }';
		customCssStore.showFor('user-a', appearance(':root { --dn-spec-order: 2px; }', site));
		expect(token('--dn-spec-order')).toBe('2px');
		expect(token('--dn-spec-site')).toBe('1px');

		customCssStore.showFor('user-a', appearance(':root { --dn-spec-order: 2px; }', site, false));
		expect(token('--dn-spec-order')).toBe('1px');

		customCssStore.hide();
		expect(token('--dn-spec-site')).toBe('');
	});

	// Last: the hatch holds the sheets off until a full reload.
	it('holds both sheets off with ?nocustomcss=1', () => {
		const address = window.location.href;
		history.replaceState(history.state, '', '?nocustomcss=1');
		try {
			customCssStore.showFor(
				'user-h',
				appearance(':root { --dn-spec-h: 1px; }', ':root { --dn-spec-h-site: 1px; }')
			);
			expect(customCssStore.disabled).toBe(true);
			expect(token('--dn-spec-h-site')).toBe('');
			expect(token('--dn-spec-h')).toBe('');
		} finally {
			history.replaceState(history.state, '', address);
		}
	});
});
