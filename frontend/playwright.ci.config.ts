import { defineConfig, devices } from '@playwright/test';

// CI browser flows (`ci-*.spec.ts`): fully mocked, no live network. Runs
// against a local Vite dev server started on demand; safe for `make ci`.
// The legacy live-stack suite keeps its own config (playwright.config.ts).
export default defineConfig({
	testDir: './tests/e2e',
	testMatch: ['ci-*.spec.ts'],
	timeout: 60_000,
	fullyParallel: false,
	retries: 0,
	reporter: 'list',
	use: {
		baseURL: 'http://127.0.0.1:5199',
		trace: 'on-first-retry'
	},
	webServer: {
		command: 'pnpm exec vite --host 127.0.0.1 --port 5199 --strictPort',
		url: 'http://127.0.0.1:5199',
		reuseExistingServer: true,
		timeout: 120_000
	},
	projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }]
});
