import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vitest/config';
import { sveltekit } from '@sveltejs/kit/vite';

// `$env/dynamic/public` reads an SSR-injected global absent in the chromium test env and throws on import; alias to an empty-env stub so component tests can load
const envPublicStub = fileURLToPath(new URL('./src/lib/test/env-public-stub.ts', import.meta.url));

export default defineConfig({
	plugins: [sveltekit()],
	test: {
		expect: { requireAssertions: true },
		projects: [
			{
				extends: true,
				resolve: {
					alias: { '$env/dynamic/public': envPublicStub }
				},
				test: {
					name: 'client',
					// Browser tests must see real Tailwind/daisyUI styles for
					// layout-dependent assertions (GH-281 sidebar scrolling); other
					// CSS stays stubbed to keep unstyled-DOM specs unchanged
					css: { include: [/src[/\\]app\.css$/] },
					environment: 'browser',
					browser: {
						enabled: true,
						headless: true,
						provider: 'playwright',
						instances: [{ browser: 'chromium' }],
						// GH runners see the default bind as internet-exposed, which disables
						// CDP/exec + failure screenshots; loopback avoids that, and the
						// browser/vitest pair must match (@vitest/browser pins vitest exactly)
						api: { host: '127.0.0.1' }
					},
					include: ['src/**/*.svelte.{test,spec}.{js,ts}'],
					exclude: ['src/lib/server/**'],
					setupFiles: ['./vitest-setup-client.ts']
				}
			},
			{
				extends: true,
				test: {
					name: 'server',
					environment: 'node',
					include: ['src/**/*.{test,spec}.{js,ts}'],
					exclude: ['src/**/*.svelte.{test,spec}.{js,ts}']
				}
			}
		]
	}
});
