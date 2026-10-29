import { describe, expect, it } from 'vitest';

import { ADMIN, SHARED, SIGNED_OUT } from './keys';

// Every key factory, found by file name, so a new factory is checked here
// without editing this spec.
const modules = import.meta.glob<Record<string, unknown>>(
	['./**/*QueryKeyFactory.ts', './**/*Keys.ts'],
	{
		eager: true
	}
);

type Builder = (...args: unknown[]) => unknown;

/** Every key builder and fixed key of every factory, by its path. */
function collect(): [string, Builder | readonly unknown[]][] {
	const found: [string, Builder | readonly unknown[]][] = [];
	const walk = (path: string, value: unknown) => {
		if (typeof value === 'function') found.push([path, value as Builder]);
		else if (Array.isArray(value)) found.push([path, value]);
		else if (value && typeof value === 'object') {
			for (const [name, inner] of Object.entries(value)) walk(`${path}.${name}`, inner);
		}
	};
	for (const [file, exports] of Object.entries(modules)) {
		for (const [name, value] of Object.entries(exports)) walk(`${file} ${name}`, value);
	}
	return found;
}

// Stand-ins for the arguments after the first: ids and text, params objects, id lists.
const STAND_INS: unknown[] = ['part', {}, ['part']];

/** Calls a builder with `first` as its first argument and a stand-in for the rest. */
function build(builder: Builder, first: string | undefined): readonly unknown[] {
	const count = Math.max(builder.length - 1, 0);
	let failure: unknown;
	for (const standIn of STAND_INS) {
		try {
			return builder(first, ...Array.from({ length: count }, () => standIn)) as readonly unknown[];
		} catch (error) {
			failure = error;
		}
	}
	throw failure;
}

const isUserScope = (scope: unknown) =>
	scope === SIGNED_OUT || (typeof scope === 'string' && scope.startsWith('user:'));

const entries = collect();
const userBuilders = entries.filter(
	(entry): entry is [string, Builder] =>
		typeof entry[1] === 'function' && isUserScope(build(entry[1], 'user-a')[0])
);

describe('query keys', () => {
	it('finds every factory', () => {
		expect(Object.keys(modules).length).toBeGreaterThan(30);
		expect(userBuilders.length).toBeGreaterThan(100);
	});

	it('start with their scope and feature', () => {
		for (const [path, entry] of entries) {
			const queryKey = typeof entry === 'function' ? build(entry, 'user-a') : entry;
			expect(Array.isArray(queryKey), path).toBe(true);
			const [scope, feature] = queryKey;
			expect(isUserScope(scope) || scope === SHARED || scope === ADMIN, path).toBe(true);
			expect(typeof feature === 'string' && feature.length > 0, path).toBe(true);
		}
	});

	it('keep two users apart, and name the user only in the scope', () => {
		for (const [path, builder] of userBuilders) {
			const a = build(builder, 'user-a');
			expect(a[0], path).toBe('user:user-a');
			expect(build(builder, 'user-b')[0], path).toBe('user:user-b');
			expect(a.slice(1), path).not.toContain('user-a');
		}
	});

	it('keep the signed-out visitor apart from every user', () => {
		for (const [path, builder] of userBuilders) {
			expect(build(builder, undefined)[0], path).toBe(SIGNED_OUT);
		}
	});
});
