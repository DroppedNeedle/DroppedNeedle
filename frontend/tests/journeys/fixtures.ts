import { test as base, expect } from '@playwright/test';

// Every journey fails if the page asks for anything off the server's origin:
// the server is sealed, so the browser must not reach out on its own either.
// Such requests are stopped, never sent.
export const test = base.extend<{ sameOriginOnly: void }>({
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
	]
});

export { expect };
