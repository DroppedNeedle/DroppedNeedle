import { defineConfig, devices } from '@playwright/test';

// Each journey runs against a fresh server that server/scripts/e2e-container.sh
// starts on a network with no way out, and the script passes its address here.
const baseURL = process.env.DN_E2E_BASE_URL;
if (!baseURL) {
	throw new Error('Run the journeys with server/scripts/e2e-container.sh (make container-e2e).');
}

export default defineConfig({
	testDir: '.',
	outputDir: '../../test-results/journeys',
	// The script runs one journey file per server.
	workers: 1,
	fullyParallel: false,
	forbidOnly: !!process.env.CI,
	retries: 0,
	reporter: 'list',
	timeout: 120_000,
	expect: { timeout: 30_000 },
	use: {
		baseURL,
		trace: 'retain-on-failure'
	},
	projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }]
});
