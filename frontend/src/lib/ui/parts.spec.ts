import { readdirSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

import { PARTS } from './parts';

// The style hooks are a published contract: every name is on the page,
// each in one component, and the docs list exactly these names.
const src = fileURLToPath(new URL('../..', import.meta.url));
const docs = fileURLToPath(new URL('../../../../docs/custom-css.md', import.meta.url));
const OWN_FILES = ['lib/ui/parts.ts', 'lib/ui/parts.spec.ts'];

const names = Object.keys(PARTS).sort();
const attribute = /data-part/g;
const setting = /^data-part=\{part\('([a-z0-9-]+)'\)\}/;

function sources(dir: string): string[] {
	return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
		const path = join(dir, entry.name);
		if (entry.isDirectory()) return sources(path);
		return /\.(ts|js|svelte)$/.test(entry.name) ? [path] : [];
	});
}

type Use = { file: string; name: string | null };

function uses(): Use[] {
	return sources(src).flatMap((path) => {
		const file = relative(src, path);
		if (OWN_FILES.includes(file)) return [];
		const text = readFileSync(path, 'utf8');
		return [...text.matchAll(attribute)].map((hit) => ({
			file,
			name: setting.exec(text.slice(hit.index))?.[1] ?? null
		}));
	});
}

function documented(): string[] {
	const text = readFileSync(docs, 'utf8');
	const section = text.split(/^## /m).find((part) => part.startsWith('Style hooks'));
	if (!section) return [];
	return [...section.matchAll(/^\| `([a-z0-9-]+)` +\|/gm)].map((row) => row[1] ?? '').sort();
}

describe('style hooks', () => {
	it('names are lowercase kebab-case', () => {
		expect(names.filter((name) => !/^[a-z]+(-[a-z]+)*$/.test(name))).toEqual([]);
	});

	it('sets every data-part through part() with a listed name', () => {
		const bad = uses().filter((use) => use.name === null || !(use.name in PARTS));
		expect(bad).toEqual([]);
	});

	it('sets each listed name in exactly one file', () => {
		const files = new Map<string, Set<string>>(names.map((name) => [name, new Set()]));
		for (const use of uses()) if (use.name) files.get(use.name)?.add(use.file);
		const wrong = names.flatMap((name) => {
			const where = [...(files.get(name) ?? [])];
			return where.length === 1 ? [] : [`${name}: ${where.length ? where.join(', ') : 'nowhere'}`];
		});
		expect(wrong).toEqual([]);
	});

	it('docs list exactly these names', () => {
		expect(documented()).toEqual(names);
	});
});
