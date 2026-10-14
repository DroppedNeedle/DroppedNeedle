import { describe, it, expect } from 'vitest';
import { parseThemePreference, resolveTheme } from './theme.svelte';

describe('theme preference', () => {
	it('defaults anything unknown to dark', () => {
		expect(parseThemePreference(null)).toBe('dark');
		expect(parseThemePreference('sepia')).toBe('dark');
		expect(parseThemePreference('light')).toBe('light');
		expect(parseThemePreference('system')).toBe('system');
	});

	it('follows the OS only for system', () => {
		expect(resolveTheme('system', true)).toBe('light');
		expect(resolveTheme('system', false)).toBe('dark');
		expect(resolveTheme('dark', true)).toBe('dark');
		expect(resolveTheme('light', false)).toBe('light');
	});
});
