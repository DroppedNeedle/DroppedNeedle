import { readdirSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

// Markup from a string goes only through SafeHtml, which sanitizes first.
// eslint refuses a raw {@html} elsewhere; this scan refuses the two ways
// around that rule: switching it off inline, and writing markup through
// the DOM.
const src = fileURLToPath(new URL('../..', import.meta.url));
const SAFE_HTML = 'lib/ui/SafeHtml.svelte';

const disablesHtmlRule = /eslint-disable[^\n]*svelte\/no-at-html-tags/;
// Spelled in parts so this file does not match itself.
const domMarkup = new RegExp(
	`\\b(?:${['inner', 'outer', 'insertAdjacent'].map((name) => `${name}HTML`).join('|')})\\b`
);

function sources(dir: string): string[] {
	return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
		const path = join(dir, entry.name);
		if (entry.isDirectory()) return sources(path);
		return /\.(ts|js|svelte)$/.test(entry.name) ? [path] : [];
	});
}

function hits(pattern: RegExp, skip: string[]): string[] {
	return sources(src).flatMap((path) => {
		const file = relative(src, path);
		if (skip.includes(file)) return [];
		return readFileSync(path, 'utf8')
			.split('\n')
			.flatMap((line, at) => (pattern.test(line) ? [`${file}:${at + 1}`] : []));
	});
}

describe('raw HTML', () => {
	it('finds the source', () => {
		expect(sources(src).map((path) => relative(src, path))).toContain(SAFE_HTML);
	});

	it('never switches the {@html} rule off inline', () => {
		expect(hits(disablesHtmlRule, [])).toEqual([]);
	});

	it('writes markup through the DOM only in SafeHtml', () => {
		expect(hits(domMarkup, [SAFE_HTML])).toEqual([]);
	});
});
