import { afterEach, describe, expect, it } from 'vitest';
import { rememberSignInReturn, returnPath, takeSignInReturn } from './signInReturn';

afterEach(() => sessionStorage.clear());

describe('sign-in return', () => {
	it('keeps only pages inside the app', () => {
		expect(returnPath('/connect?client_id=a&state=b')).toBe('/connect?client_id=a&state=b');
		expect(returnPath('/library/albums')).toBe('/library/albums');
		for (const path of [
			'',
			'/',
			'//evil.example/x',
			'/\\evil.example',
			'https://evil.example/',
			'javascript:alert(1)',
			'/login',
			'/login?local',
			'/auth/callback?code=x',
			'/setup',
			'/x\ny'
		]) {
			expect(returnPath(path), path).toBeNull();
		}
	});

	it('returns once to the remembered page, then home', () => {
		rememberSignInReturn('/connect?state=s', 1_000);
		expect(takeSignInReturn(2_000)).toBe('/connect?state=s');
		expect(takeSignInReturn(2_000)).toBe('/');
	});

	it('forgets a stale or unsafe address', () => {
		rememberSignInReturn('/connect', 0);
		expect(takeSignInReturn(15 * 60 * 1000 + 1)).toBe('/');
		rememberSignInReturn('/connect', 0);
		rememberSignInReturn('//evil.example', 0);
		expect(takeSignInReturn(1)).toBe('/');
		sessionStorage.setItem(
			'droppedneedle.signInReturn',
			JSON.stringify({ path: '//evil.example', at: 0 })
		);
		expect(takeSignInReturn(1)).toBe('/');
		sessionStorage.setItem('droppedneedle.signInReturn', 'not json');
		expect(takeSignInReturn(1)).toBe('/');
	});
});
