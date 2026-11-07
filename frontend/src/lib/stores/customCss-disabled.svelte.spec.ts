import { describe, expect, it, vi } from 'vitest';

const KEY = 'droppedneedle_custom_css';

history.replaceState(null, '', '/?nocustomcss=1');
vi.resetModules();

describe('custom css disabled', () => {
	it('does not apply the sheet', async () => {
		const { customCssStore } = await import('./customCss.svelte');
		const before = document.adoptedStyleSheets.length;

		customCssStore.set(':root { --dn-test: 1; }');

		expect(customCssStore.disabled).toBe(true);
		expect(localStorage.getItem(KEY)).toContain('--dn-test');
		expect(document.adoptedStyleSheets.length).toBe(before);

		customCssStore.clear();
	});
});
