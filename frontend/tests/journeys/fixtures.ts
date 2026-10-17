import { test as base, expect } from '@playwright/test';

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
