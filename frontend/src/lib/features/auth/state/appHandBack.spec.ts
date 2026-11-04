import { describe, expect, it } from 'vitest';
import { appAddress } from './appHandBack';

describe('appAddress', () => {
	it('lets through only what the server lists for apps', () => {
		for (const value of [
			'com.example.player:/connect?code=a&state=b',
			'https://app.example/back?code=a'
		]) {
			expect(appAddress(value), value).toBe(value);
		}
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
