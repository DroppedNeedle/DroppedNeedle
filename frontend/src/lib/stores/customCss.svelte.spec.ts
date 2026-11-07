import { beforeEach, describe, expect, it, vi } from 'vitest';

const KEY = 'droppedneedle_custom_css';

describe('custom css store', () => {
	beforeEach(() => {
		vi.resetModules();
		history.replaceState(null, '', '/');
		localStorage.removeItem(KEY);
		document.adoptedStyleSheets = [];
	});

	it('applies and removes the sheet', async () => {
		const { customCssStore } = await import('./customCss.svelte');
		customCssStore.clear();
		const before = document.adoptedStyleSheets.length;
		const beforeSheets = new Set(document.adoptedStyleSheets);

		customCssStore.set(':root { --dn-test: 1; }');

		expect(localStorage.getItem(KEY)).toContain('--dn-test');
		expect(document.adoptedStyleSheets.length).toBe(before + 1);
		const added = document.adoptedStyleSheets.find((sheet) => !beforeSheets.has(sheet));
		expect(added?.cssRules[0]?.cssText).toContain('--dn-test');

		customCssStore.clear();

		expect(localStorage.getItem(KEY)).toBeNull();
		expect(document.adoptedStyleSheets.length).toBe(before);
	});
});
