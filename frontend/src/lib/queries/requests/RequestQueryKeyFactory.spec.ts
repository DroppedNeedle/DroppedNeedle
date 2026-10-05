import { describe, expect, it } from 'vitest';

import { RequestQueryKeyFactory } from './RequestQueryKeyFactory';

describe('RequestQueryKeyFactory', () => {
	it('nests every key under the shared requests prefix', () => {
		expect(RequestQueryKeyFactory.active('userA').slice(0, 1)).toEqual([
			...RequestQueryKeyFactory.all
		]);
		expect(RequestQueryKeyFactory.history('userA', {}).slice(0, 1)).toEqual([
			...RequestQueryKeyFactory.all
		]);
		expect(RequestQueryKeyFactory.approvals('userA').slice(0, 1)).toEqual([
			...RequestQueryKeyFactory.all
		]);
	});

	it('carries a userId segment on user-dependent keys', () => {
		expect(RequestQueryKeyFactory.active('userA')).toContain('userA');
		expect(RequestQueryKeyFactory.activeCount('userA')).toContain('userA');
		expect(RequestQueryKeyFactory.history('userA', {})).toContain('userA');
		expect(RequestQueryKeyFactory.approvals('userA')).toContain('userA');
	});

	it('produces different keys for different users', () => {
		expect(RequestQueryKeyFactory.active('userA')).not.toEqual(
			RequestQueryKeyFactory.active('userB')
		);
		expect(RequestQueryKeyFactory.history('userA', {})).not.toEqual(
			RequestQueryKeyFactory.history('userB', {})
		);
	});

	it('keys history pages separately by params', () => {
		expect(RequestQueryKeyFactory.history('userA', { page: 1 })).not.toEqual(
			RequestQueryKeyFactory.history('userA', { page: 2 })
		);
		expect(RequestQueryKeyFactory.history('userA', { status: 'failed' })).not.toEqual(
			RequestQueryKeyFactory.history('userA', { status: 'imported' })
		);
	});

	it('lets one prefix invalidation sweep active, history, and approvals', () => {
		const prefix = [...RequestQueryKeyFactory.all];
		for (const key of [
			RequestQueryKeyFactory.active('userA'),
			RequestQueryKeyFactory.activeCount('userA'),
			RequestQueryKeyFactory.history('userA', {}),
			RequestQueryKeyFactory.approvals('userA')
		]) {
			expect([...key].slice(0, prefix.length)).toEqual(prefix);
		}
	});
});
