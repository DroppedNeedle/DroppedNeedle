import { describe, expect, it } from 'vitest';
import { MAX_CUSTOM_CSS, clampCustomCss, isCustomCssDisabled } from './customCss.svelte';

describe('custom css', () => {
	it('recognises the escape hatch', () => {
		expect(isCustomCssDisabled('?nocustomcss=1')).toBe(true);
		expect(isCustomCssDisabled('?foo=bar&nocustomcss=1')).toBe(true);
		expect(isCustomCssDisabled('?nocustomcss=0')).toBe(false);
		expect(isCustomCssDisabled('')).toBe(false);
	});

	it('caps the stored css', () => {
		expect(clampCustomCss('a'.repeat(MAX_CUSTOM_CSS + 10))).toHaveLength(MAX_CUSTOM_CSS);
		expect(clampCustomCss('a')).toBe('a');
	});
});
