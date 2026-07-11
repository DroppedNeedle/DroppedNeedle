import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

// SvelteKit requires base to start with `/` and not end with one; empty = domain root.
function normalizeBase(value) {
	const trimmed = (value ?? '').trim().replace(/\/+$/, '');
	if (!trimmed) return '';
	return trimmed.startsWith('/') ? trimmed : `/${trimmed}`;
}

/** @type {import('@sveltejs/kit').Config} */
const config = {
	preprocess: vitePreprocess(),

	kit: {
		adapter: adapter({
			pages: 'build',
			assets: 'build',
			fallback: 'index.html',
			precompress: true
		}),
		paths: {
			base: normalizeBase(process.env.BASE_PATH)
		},
		appDir: '_app'
	}
};

export default config;
