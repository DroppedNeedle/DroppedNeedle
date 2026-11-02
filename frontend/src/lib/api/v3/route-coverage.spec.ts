import { readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { extractV3Templates } from './registryScan';
import { ROUTE_REASONS } from './route-reasons';

/**
 * Every path in the API document has a caller in the web app or a reason in
 * `route-reasons.ts`. The check is per path: a method nobody calls on a path
 * that has a caller is left to review.
 */

const here = dirname(fileURLToPath(import.meta.url));
const document = JSON.parse(
	readFileSync(join(here, '..', '..', '..', '..', '..', 'server', 'openapi', 'openapi.json'), 'utf8')
) as { paths: Record<string, Record<string, unknown>> };

const srcRoot = join(here, '..', '..', '..');
const called = new Set(
	(readdirSync(srcRoot, { recursive: true, encoding: 'utf8' }) as string[])
		.filter((entry) => entry.endsWith('.ts') || entry.endsWith('.svelte'))
		.filter((entry) => !entry.endsWith('.spec.ts') && !entry.endsWith('.test.ts'))
		.filter((entry) => statSync(join(srcRoot, entry)).isFile())
		.flatMap((entry) => extractV3Templates(readFileSync(join(srcRoot, entry), 'utf8')))
);
const reasoned = ROUTE_REASONS.map((row) => row.path);

describe('v3 route coverage', () => {
	it('gives every document path a web caller or a reason', () => {
		const uncovered = Object.keys(document.paths).filter(
			(path) => !called.has(path as never) && !reasoned.includes(path)
		);
		expect(uncovered).toEqual([]);
	});

	it('lists only operations the document has', () => {
		const gone = ROUTE_REASONS.filter(
			(row) => !document.paths[row.path]?.[row.method.toLowerCase()]
		);
		expect(gone).toEqual([]);
	});
});
