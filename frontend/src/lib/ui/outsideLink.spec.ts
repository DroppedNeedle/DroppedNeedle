import { describe, expect, it } from 'vitest';

import { outsideLink, PROSE_LINK_URL } from './outsideLink';

const REFUSED = [
	'javascript:alert(1)',
	'JaVaScRiPt:alert(1)',
	' javascript:alert(1)',
	'\tjavascript:alert(1)',
	'\njavascript:alert(1)',
	'\u0000javascript:alert(1)',
	'\u0001javascript:alert(1)',
	'java\tscript:alert(1)',
	'data:text/html,<script>alert(1)</script>',
	'vbscript:msgbox(1)',
	'//example.org/path',
	'/settings',
	'settings?tab=users',
	'#top',
	' https://example.org/',
	'\u0000https://example.org/',
	'https:example.org',
	'ftp://example.org/',
	'mailto:someone@example.org',
	''
];

describe('outsideLink', () => {
	it('passes only absolute web addresses that parse, and prose adds email alone', () => {
		for (const address of [...REFUSED, 'https://', 'http://[', null, undefined]) {
			expect(outsideLink(address), JSON.stringify(address)).toBeUndefined();
		}
		for (const address of [
			'https://example.org/a?b=c#d',
			'http://example.org/',
			'HTTPS://EXAMPLE.ORG/'
		]) {
			expect(outsideLink(address)).toBe(address);
		}

		expect(PROSE_LINK_URL.test('mailto:someone@example.org')).toBe(true);
		expect(PROSE_LINK_URL.test('https://example.org/')).toBe(true);
		for (const address of REFUSED.filter((address) => !address.startsWith('mailto:'))) {
			expect(PROSE_LINK_URL.test(address), JSON.stringify(address)).toBe(false);
		}
	});
});
