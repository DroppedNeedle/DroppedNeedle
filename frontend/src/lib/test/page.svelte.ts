// A reactive stand-in for `$app/state`'s page and `goto`, for specs of
// pages that keep their state in the address: `goto` moves `testPage.url`,
// so the page reads its new address as it would in the browser.
import { vi } from 'vitest';

class TestPage {
	url = $state(new URL('http://localhost/'));
	params: Record<string, string> = {};
}

export const testPage = new TestPage();

export const testGoto = vi.fn(async (to: string | URL) => {
	// Replaced whole, never changed in place, so a plain URL is enough.
	// eslint-disable-next-line svelte/prefer-svelte-reactivity
	testPage.url = new URL(String(to), testPage.url);
});
