import { test as base, expect, type Page } from '@playwright/test';

// Every journey fails if the page asks for anything off the server's origin:
// the server is sealed, so the browser must not reach out on its own either.
// Such requests are stopped, never sent.
//
// Every journey also fails if the browser refuses anything under the
// server's content-security policy. `vite dev` sends no policy, so this is
// where a refused script, style, picture or frame shows up.
export const test = base.extend<{ sameOriginOnly: void; noPolicyViolations: void }>({
	sameOriginOnly: [
		async ({ context, baseURL }, use) => {
			const origin = new URL(baseURL ?? '').origin;
			const offOrigin: string[] = [];
			await context.route(
				(url) => url.origin !== origin,
				(route) => {
					offOrigin.push(route.request().url());
					return route.abort('blockedbyclient');
				}
			);
			await use();
			expect(offOrigin, 'requests off the server origin').toEqual([]);
		},
		{ auto: true }
	],
	noPolicyViolations: [
		async ({ context }, use) => {
			const violations: string[] = [];
			await context.exposeBinding('__reportPolicyViolation', ({ page }, violation: string) => {
				violations.push(`${violation} on ${page.url()}`);
			});
			await context.addInitScript(() => {
				document.addEventListener('securitypolicyviolation', (event) => {
					const report = (window as unknown as { __reportPolicyViolation?: (v: string) => void })
						.__reportPolicyViolation;
					let blocked = event.blockedURI || '(none)';
					try {
						blocked = new URL(blocked).host || blocked;
					} catch {
						// a keyword such as inline, eval, data or blob
					}
					const sample = event.sample ? ` "${event.sample.slice(0, 60)}"` : '';
					report?.(
						`${event.effectiveDirective} refused ${blocked}` +
							` from ${event.sourceFile || 'the page'}:${event.lineNumber}${sample}`
					);
				});
			});
			await use();
			expect(violations, 'content-security policy violations').toEqual([]);
		},
		{ auto: true }
	]
});

export { expect };

export const admin = {
	displayName: 'Journey Admin',
	username: 'journey-admin',
	password: 'correct horse battery staple'
};

// The mock MusicBrainz and slskd the script starts on the same sealed
// network, as the server reaches them.
export const mocks = {
	musicbrainz: requireEnv('DN_E2E_MOCK_MUSICBRAINZ'),
	slskd: requireEnv('DN_E2E_MOCK_SLSKD')
};

function requireEnv(name: string): string {
	const value = process.env[name];
	if (!value) {
		throw new Error(`${name} is not set: run the journeys with server/scripts/e2e-container.sh.`);
	}
	return value;
}

// Every journey runs on a server of its own, so each starts by creating the
// first admin, which signs it in.
export async function setUpAdmin(page: Page): Promise<void> {
	await page.goto('/setup');
	await page.getByLabel('Display Name').fill(admin.displayName);
	await page.getByLabel('Username').fill(admin.username);
	await page.getByLabel('Password', { exact: true }).fill(admin.password);
	await page.getByLabel('Confirm Password').fill(admin.password);
	await page.getByRole('button', { name: 'Create Admin Account' }).click();
	await expect(page.getByRole('button', { name: 'Log out' })).toBeVisible();
}
