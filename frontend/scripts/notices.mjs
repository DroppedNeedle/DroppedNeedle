// Writes the licence notices for the npm packages in the web UI build, then
// removes the build's source maps, which only this script reads.
//
// Run from frontend/ after `pnpm run build --sourcemap hidden`:
//   node scripts/notices.mjs build NOTICES-web-ui.txt
//
// A package is in the build when a source map names one of its files, or when a
// stylesheet in src/ imports it or loads it as a Tailwind plugin. Vite is in
// every build too: it adds its preload helper, which no source map names.
import { readdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const [buildDir, outFile] = process.argv.slice(2);
if (!buildDir || !outFile) {
	console.error('usage: node scripts/notices.mjs <build folder> <notices file>');
	process.exit(2);
}

const ROOT = fileURLToPath(new URL('..', import.meta.url));
const NODE_MODULES = 'node_modules/';
const packages = new Map();

function add(dir) {
	const real = realpathSync(dir);
	if (!packages.has(real)) {
		packages.set(real, JSON.parse(readFileSync(join(real, 'package.json'), 'utf8')));
	}
}

// A package name from a path or import below node_modules: `svelte/src/index.js`
// is svelte, `@tailwindcss/typography` keeps its scope.
function packageName(path) {
	const parts = path.split('/');
	return parts[0].startsWith('@') ? parts.slice(0, 2).join('/') : parts[0];
}

let maps = 0;
for (const file of readdirSync(buildDir, { recursive: true })) {
	if (!file.endsWith('.map')) continue;
	const path = join(buildDir, file);
	for (const source of JSON.parse(readFileSync(path, 'utf8')).sources) {
		const at = source.indexOf(NODE_MODULES);
		if (at === -1) continue;
		// The map was moved into the build after it was written, so its relative
		// paths are read from node_modules down.
		const below = source.slice(at);
		const last = below.lastIndexOf(NODE_MODULES) + NODE_MODULES.length;
		add(join(ROOT, below.slice(0, last), packageName(below.slice(last))));
	}
	rmSync(path);
	maps += 1;
}
if (maps === 0) {
	console.error(`no source maps in ${buildDir}: build with --sourcemap hidden`);
	process.exit(1);
}
if (packages.size === 0) {
	console.error(`the source maps in ${buildDir} name no package`);
	process.exit(1);
}

const BARE_IMPORT = /@(?:import|plugin)\s+['"]([^'"./][^'"]*)['"]/g;
for (const file of readdirSync(join(ROOT, 'src'), { recursive: true })) {
	if (!file.endsWith('.css')) continue;
	for (const [, spec] of readFileSync(join(ROOT, 'src', file), 'utf8').matchAll(BARE_IMPORT)) {
		add(join(ROOT, NODE_MODULES, packageName(spec)));
	}
}
add(join(ROOT, NODE_MODULES, 'vite'));

const LICENCE_FILE = /^(licen[cs]e|copying|copyright|notice)([-._].*)?$/i;

function licenceTexts(dir) {
	return readdirSync(dir, { withFileTypes: true })
		.filter((entry) => entry.isFile() && LICENCE_FILE.test(entry.name))
		.map((entry) => entry.name)
		.sort()
		.map((name) => readFileSync(join(dir, name), 'utf8').trim());
}

function personName(person) {
	return typeof person === 'string' ? person : person?.name;
}

const entries = [...packages]
	.map(([dir, pkg]) => ({
		name: pkg.name,
		version: pkg.version,
		licence: (typeof pkg.license === 'string' ? pkg.license : pkg.license?.type) ?? 'not stated',
		author: personName(pkg.author),
		texts: licenceTexts(dir)
	}))
	.sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version));

const lines = [
	'Packages in the DroppedNeedle web UI',
	'====================================',
	'',
	'The web UI in this image is built from the npm packages below: their code or',
	"styles are in the files it serves. Each package's licence files are given in",
	'full. A package that holds none is named with its licence and author. The',
	'address beside a package is its published source on npm.',
	'',
	'Packages:',
	...entries.map((entry) => `  ${entry.name} ${entry.version}: ${entry.licence}`)
];
for (const entry of entries) {
	lines.push(
		'',
		'-'.repeat(80),
		`${entry.name} ${entry.version}`,
		`Licence: ${entry.licence}`,
		`Source: https://www.npmjs.com/package/${entry.name}/v/${entry.version}`
	);
	if (entry.texts.length === 0) {
		lines.push(`Author: ${entry.author ?? 'not stated'}`, '', 'The package holds no licence file.');
	}
	for (const text of entry.texts) lines.push('', text);
}
lines.push(
	'',
	'-'.repeat(80),
	"The web UI's fonts are under the SIL Open Font License 1.1. Their licence files",
	'are beside them, in /app/static/fonts/.'
);
writeFileSync(outFile, `${lines.join('\n')}\n`);
console.log(`${outFile}: ${entries.length} packages from ${maps} source maps`);
