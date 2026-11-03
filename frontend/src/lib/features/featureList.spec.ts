import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

// eslint.config.js builds the feature boundary rule from public.json: a feature
// missing from it has no rule, and a listed path that moved or went away
// would let nothing through, or the wrong module.
const here = fileURLToPath(new URL('.', import.meta.url));
const publicModules: Record<string, string[]> = JSON.parse(
	readFileSync(`${here}/public.json`, 'utf8')
);
const folders = readdirSync(here, { withFileTypes: true })
	.filter((entry) => entry.isDirectory())
	.map((entry) => entry.name);

describe('the feature boundary list', () => {
	it('names exactly the folders under lib/features', () => {
		expect(folders.length).toBeGreaterThan(0);
		expect(Object.keys(publicModules).sort()).toEqual([...folders].sort());
	});

	it('lists only modules that exist', () => {
		const missing = Object.entries(publicModules).flatMap(([feature, paths]) =>
			paths.map((path) => `${feature}/${path}`).filter((path) => !existsSync(`${here}/${path}`))
		);
		expect(missing).toEqual([]);
	});
});
