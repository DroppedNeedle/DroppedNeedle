import { describe, it, expect } from 'vitest';
import { getCoverUrl, isAbortError } from './errorHandling';

describe('isAbortError', () => {
	it('returns true for Error named AbortError', () => {
		expect.assertions(1);
		const error = new Error('aborted');
		error.name = 'AbortError';
		expect(isAbortError(error)).toBe(true);
	});

	it('returns true for DOMException AbortError when available', () => {
		expect.assertions(1);
		if (typeof DOMException === 'undefined') {
			expect(isAbortError(new Error('no domexception'))).toBe(false);
			return;
		}
		expect(isAbortError(new DOMException('aborted', 'AbortError'))).toBe(true);
	});

	it('returns false for non-abort errors', () => {
		expect.assertions(1);
		expect(isAbortError(new Error('other'))).toBe(false);
	});
});

describe('getCoverUrl', () => {
	const validMbid = '12345678-1234-1234-1234-123456789abc';

	it('preserves explicit local artwork when the stable catalog ID is UUID shaped', () => {
		const local = `/api/v1/library/albums/${validMbid}/artwork/local`;
		expect(getCoverUrl(local, validMbid)).toBe(local);
	});

	it('returns API URL when albumId is a valid MBID', () => {
		expect.assertions(1);
		expect(getCoverUrl(null, validMbid)).toBe(`/api/v1/covers/release-group/${validMbid}?size=250`);
	});

	it('ignores coverUrl when albumId is a valid MBID', () => {
		expect.assertions(1);
		expect(getCoverUrl('/some/custom/url.jpg', validMbid)).toBe(
			`/api/v1/covers/release-group/${validMbid}?size=250`
		);
	});

	it('returns coverUrl when albumId is not a valid MBID and coverUrl is provided', () => {
		expect.assertions(1);
		expect(getCoverUrl('/custom.jpg', 'not-a-uuid')).toBe('/custom.jpg');
	});

	it('returns API fallback URL when albumId is not a valid MBID and coverUrl is null', () => {
		expect.assertions(1);
		expect(getCoverUrl(null, 'not-a-uuid')).toBe(
			'/api/v1/covers/release-group/not-a-uuid?size=250'
		);
	});

	it('returns API fallback URL when albumId is not a valid MBID and coverUrl is undefined', () => {
		expect.assertions(1);
		expect(getCoverUrl(undefined, 'not-a-uuid')).toBe(
			'/api/v1/covers/release-group/not-a-uuid?size=250'
		);
	});

	it('returns API fallback URL when coverUrl is empty string', () => {
		expect.assertions(1);
		expect(getCoverUrl('', 'not-a-uuid')).toBe('/api/v1/covers/release-group/not-a-uuid?size=250');
	});

	it('always returns a non-empty string', () => {
		expect.assertions(3);
		expect(getCoverUrl(null, validMbid)).toBeTruthy();
		expect(getCoverUrl(null, 'invalid')).toBeTruthy();
		expect(getCoverUrl('/img.jpg', 'invalid')).toBeTruthy();
	});
});
