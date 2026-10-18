import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import MetadataPanel from './MetadataPanel.svelte';
import { outsideLink, PROSE_LINK_URL } from './outsideLink';

type Flagged = { __xss?: string };
const flagged = window as unknown as Flagged;

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
	it('refuses every address that is not an absolute web address', () => {
		for (const address of REFUSED) {
			expect(outsideLink(address), JSON.stringify(address)).toBeUndefined();
		}
		expect(outsideLink(null)).toBeUndefined();
		expect(outsideLink(undefined)).toBeUndefined();
	});

	it('passes web addresses through unchanged', () => {
		for (const address of [
			'https://example.org/a?b=c#d',
			'http://example.org/',
			'HTTPS://EXAMPLE.ORG/'
		]) {
			expect(outsideLink(address)).toBe(address);
		}
	});

	it('lets prose link an email address and nothing else extra', () => {
		expect(PROSE_LINK_URL.test('mailto:someone@example.org')).toBe(true);
		expect(PROSE_LINK_URL.test('https://example.org/')).toBe(true);
		for (const address of REFUSED.filter((address) => !address.startsWith('mailto:'))) {
			expect(PROSE_LINK_URL.test(address), JSON.stringify(address)).toBe(false);
		}
	});
});

describe('an address from a connected media server', () => {
	afterEach(() => {
		delete flagged.__xss;
	});

	it('is not rendered as a link when it could run script', async () => {
		await render(MetadataPanel, {
			props: {
				open: true,
				title: 'Blue Lines',
				lastfmUrl: "javascript:window.__xss = 'lastfm'",
				musicbrainzId: '0a1b2c3d-0000-4000-8000-000000000000',
				onclose: () => {}
			}
		});

		await expect.element(page.getByRole('link', { name: 'MusicBrainz' })).toBeVisible();
		await expect.element(page.getByRole('link', { name: 'Last.fm' })).not.toBeInTheDocument();
		expect(document.body.querySelector('a[href^="javascript" i]')).toBeNull();
	});

	it('is rendered as a link when it is a web address', async () => {
		await render(MetadataPanel, {
			props: {
				open: true,
				title: 'Blue Lines',
				lastfmUrl: 'https://www.last.fm/music/Massive+Attack/Blue+Lines',
				onclose: () => {}
			}
		});

		const link = page.getByRole('link', { name: 'Last.fm' });
		await expect
			.element(link)
			.toHaveAttribute('href', 'https://www.last.fm/music/Massive+Attack/Blue+Lines');
		await expect.element(link).toHaveAttribute('target', '_blank');
		await expect.element(link).toHaveAttribute('rel', 'noopener noreferrer');
	});
});
