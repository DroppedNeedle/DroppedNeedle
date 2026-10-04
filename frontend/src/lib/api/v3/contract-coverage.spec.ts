import { readdirSync, readFileSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { countV3Calls, extractV3Templates } from './registryScan';

/**
 * Registry coverage without hand-kept rows: every v3 template used in shipped
 * code under src must be a route in the generated OpenAPI snapshot (the same
 * file the contract drift gate verifies), and every builder call must spell
 * its template as a string literal so this scan cannot miss it. One case per
 * route. Specs are excluded (they may pin invalid templates on purpose);
 * svelte-check independently rejects unknown templates at build time.
 */

const here = dirname(fileURLToPath(import.meta.url));
const specJson = JSON.parse(
	readFileSync(join(here, '..', '..', '..', '..', '..', 'server', 'openapi', 'openapi.json'), 'utf8')
) as { paths: Record<string, unknown> };
const specPaths = new Set(Object.keys(specJson.paths));

const srcRoot = join(here, '..', '..', '..');
const sources = (
	readdirSync(srcRoot, { recursive: true, encoding: 'utf8' }) as string[]
)
	.filter((entry) => entry.endsWith('.ts') || entry.endsWith('.svelte'))
	.filter((entry) => !entry.endsWith('.spec.ts') && !entry.endsWith('.test.ts'))
	.filter((entry) => statSync(join(srcRoot, entry)).isFile())
	.map((entry) => readFileSync(join(srcRoot, entry), 'utf8'));

const usedTemplates = [...new Set(sources.flatMap(extractV3Templates))].sort();
const literalCount = sources.reduce((total, source) => total + extractV3Templates(source).length, 0);
const callCount = sources.reduce((total, source) => total + countV3Calls(source), 0);

describe('v3 registry coverage', () => {
	it('reads a non-empty generated route set', () => {
		expect(specPaths.size).toBeGreaterThan(0);
	});

	it('spells every v3 template as a scannable literal', () => {
		expect(sources.length).toBeGreaterThan(100);
		expect(literalCount).toBe(callCount);
	});

	it.each(usedTemplates)('%s is a generated route', (template) => {
		expect(specPaths.has(template)).toBe(true);
	});
});
