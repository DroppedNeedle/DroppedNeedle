// Holds what a signed-in person's browser loads to show Home to a budget.
//
// Run from frontend/ after `pnpm run build`:
//   node scripts/home-budget.mjs
//
// It reads the build's Vite manifest and follows the static imports of the
// start and app entries, the root layout, the Home page and the app shell
// (which the root layout imports once signed in). It fails when those files
// pass the file or brotli budget below, or when one of them holds a library
// that only a modal opened on demand should load.
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { constants, brotliCompressSync } from 'node:zlib';

// The numbers measured when this check was added, plus about 10%. Raise one
// only for a change that has to load on Home; say why in its commit.
const MAX_FILES = 145;
const MAX_BROTLI_BYTES = 310_000;

// Libraries kept off Home, each known by a string its build always keeps.
const KEPT_OFF = [
	{ name: 'marked', mark: 'input parameter is undefined or null' },
	{ name: 'dompurify', mark: 'SAFE_FOR_TEMPLATES' }
];

const OUTPUT = '.svelte-kit/output/client';
const NODES = '.svelte-kit/generated/client-optimized/nodes';
const SHELL = 'src/lib/shell/AuthenticatedAppShell.svelte';

const manifest = JSON.parse(readFileSync(join(OUTPUT, '.vite/manifest.json'), 'utf8'));

// The generated node file for a route component names its source.
function nodeFor(component) {
	for (const file of readdirSync(NODES)) {
		if (readFileSync(join(NODES, file), 'utf8').includes(`/${component}"`)) {
			return `${NODES}/${file}`;
		}
	}
	throw new Error(`no route node loads ${component}`);
}

// Every route node is an entry too; the page itself starts from these two.
const START = Object.keys(manifest).find((key) => key.endsWith('/kit/src/runtime/client/entry.js'));
if (!START) throw new Error('the manifest has no start entry');

const roots = [
	START,
	'.svelte-kit/generated/client-optimized/app.js',
	nodeFor('src/routes/+layout.svelte'),
	nodeFor('src/routes/+page.svelte'),
	SHELL
];

const files = new Set();
const seen = new Set();
function follow(key) {
	if (seen.has(key)) return;
	seen.add(key);
	const chunk = manifest[key];
	if (!chunk) throw new Error(`the manifest has no ${key}`);
	files.add(chunk.file);
	for (const css of chunk.css ?? []) files.add(css);
	for (const imported of chunk.imports ?? []) follow(imported);
}
roots.forEach(follow);

let js = 0;
let css = 0;
let brotli = 0;
const found = new Set();
for (const file of files) {
	const bytes = readFileSync(join(OUTPUT, file));
	if (file.endsWith('.css')) css += 1;
	else js += 1;
	brotli += brotliCompressSync(bytes, {
		params: { [constants.BROTLI_PARAM_QUALITY]: constants.BROTLI_MAX_QUALITY }
	}).length;
	const text = bytes.toString('utf8');
	for (const library of KEPT_OFF) if (text.includes(library.mark)) found.add(library.name);
}

const total = js + css;
console.log(
	`signed-in Home: ${total} files (${js} JS, ${css} CSS), ${(brotli / 1024).toFixed(1)} KB brotli`
);
console.log(`budget: ${MAX_FILES} files, ${(MAX_BROTLI_BYTES / 1024).toFixed(1)} KB brotli`);

const failures = [];
if (total > MAX_FILES) failures.push(`${total} files is over the budget of ${MAX_FILES}`);
if (brotli > MAX_BROTLI_BYTES) {
	failures.push(`${brotli} brotli bytes is over the budget of ${MAX_BROTLI_BYTES}`);
}
for (const name of found) failures.push(`${name} is loaded on Home`);
for (const failure of failures) console.error(`FAIL: ${failure}`);
process.exit(failures.length > 0 ? 1 : 0);
