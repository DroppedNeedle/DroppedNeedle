import { describe, expect, it } from 'vitest';
import { appAddress } from './appHandBack';

describe('appAddress', () => {
	it('lets through what the server lists for apps', () => {
		expect(appAddress('com.example.player:/connect?code=a&state=b')).toBe(
			'com.example.player:/connect?code=a&state=b'
		);
		expect(appAddress('https://app.example/back?code=a')).toBe('https://app.example/back?code=a');
	});

	it('refuses addresses that would run or show something here', () => {
		for (const value of [
			'javascript:alert(1)',
			'JavaScript:alert(1)',
			'data:text/html,x',
			'http://app.example/back',
			'/connect',
			' com.example.player:/connect',
			'',
			null,
			42
		]) {
			expect(appAddress(value), String(value)).toBeNull();
		}
	});
});
