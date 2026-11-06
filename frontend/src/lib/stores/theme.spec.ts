import { describe, expect, it } from 'vitest';
import { parseThemePreference, resolveTheme } from './theme.svelte';

describe('theme preference', () => {
	it('keeps midnight as a stored choice', () => {
		expect(parseThemePreference('midnight')).toBe('midnight');
	});

	it('resolves midnight without the system query', () => {
		expect(resolveTheme('midnight', true)).toBe('midnight');
		expect(resolveTheme('midnight', false)).toBe('midnight');
	});

	it('resolves system to the device preference', () => {
		expect(resolveTheme('system', true)).toBe('light');
		expect(resolveTheme('system', false)).toBe('dark');
	});
});
