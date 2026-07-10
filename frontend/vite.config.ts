import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vitest/config';
import { sveltekit } from '@sveltejs/kit/vite';

// `$env/dynamic/public` reads an SSR-injected global absent in the chromium test env and throws on import; alias to an empty-env stub so component tests can load
const envPublicStub = fileURLToPath(new URL('./src/lib/test/env-public-stub.ts', import.meta.url));

export default defineConfig({
	plugins: [sveltekit()],
	test: {
		expect: { requireAssertions: true },
		// Dynamic import() in specs can stall >10s while the vite module runner
		// transforms under parallel cross-project load; give hooks/tests headroom.
		testTimeout: 30000,
		hookTimeout: 30000,
		// The browser-mode vite server resolves its port from THIS top level
		// (test.browser.api), not from the project entry below; without it vitest
		// falls back to 63315, which sits in a Windows excluded TCP port range on
		// some machines (EACCES). Pin a safe port so the client suite can boot.
		browser: { api: { port: 24817 } },
		projects: [
			{
				extends: true,
				resolve: {
					alias: { '$env/dynamic/public': envPublicStub }
				},
				test: {
					name: 'client',
					environment: 'browser',
					browser: {
						enabled: true,
						headless: true,
						provider: 'playwright',
						instances: [{ browser: 'chromium' }]
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
