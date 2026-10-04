import { describe, expect, it } from 'vitest';
import { userIdSegment } from './userKeySegment';

describe('userIdSegment', () => {
	it('normalizes a missing userId to null', () => {
		expect(userIdSegment(undefined)).toBeNull();
	});

	it('keeps an explicit null as null', () => {
		expect(userIdSegment(null)).toBeNull();
	});

	it('keeps the userId string', () => {
		expect(userIdSegment('user-a')).toBe('user-a');
	});

	it('gives distinct users distinct segments', () => {
		expect(userIdSegment('user-a')).not.toBe(userIdSegment('user-b'));
	});
});
