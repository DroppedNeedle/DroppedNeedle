import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import MetadataPanel from './MetadataPanel.svelte';
import WhatsNewModal from './WhatsNewModal.svelte';

type Flagged = { __xss?: string };
const flagged = window as unknown as Flagged;

const HOSTILE_PARTS = [
	'<img src="x" onerror="window.__xss = \'img\'">',
	'<img src="data:image/gif;base64,R0lGODlhAQABAAAAACw=" alt="inline image">',
	"<script>window.__xss = 'script'</script>",
	'<a href="javascript:window.__xss = \'link\'">hostile link</a>',
	'<iframe srcdoc="<script>parent.__xss = \'frame\'</script>"></iframe>',
	'<svg><a xlink:href="javascript:window.__xss = \'svg\'"><text>svg link</text></a></svg>'
].join('');

// A well-formed remote image: allowed in our own release notes, never in
// third-party notes.
const REMOTE_IMAGE = '<img src="https://example.invalid/cover.png" alt="remote image">';

const HOSTILE_HTML =
	'<p>Kept paragraph with <strong>bold</strong> and <a href="https://example.org/kept">a kept link</a>.</p>' +
	REMOTE_IMAGE +
	HOSTILE_PARTS;

const HOSTILE_MARKDOWN = [
	'## Kept heading',
	'',
	'Kept paragraph with **bold** and [a kept link](https://example.org/kept).',
	'',
	"[markdown link](javascript:window.__xss='mdlink')",
	'',
	REMOTE_IMAGE,
	'',
	HOSTILE_PARTS
].join('\n');

// A broken image of our own, started after the hostile one, so its error
// handler would have fired first had it survived.
function brokenImageSettled(): Promise<void> {
	return new Promise((resolve) => {
		const probe = new Image();
		probe.onerror = () => resolve();
		probe.src = 'x';
	});
}

async function expectNothingRunnable() {
	const kept = page.getByRole('link', { name: 'a kept link' });
	await expect.element(kept).toBeVisible();
	await expect.element(page.getByText('bold', { exact: true })).toBeVisible();

	const nodes = [...document.body.querySelectorAll('*')];
	const handlers = nodes.flatMap((node) =>
		node.getAttributeNames().filter((name) => name.toLowerCase().startsWith('on'))
	);
	expect(handlers).toEqual([]);
	expect(document.body.querySelectorAll('script, iframe, object, embed, svg a').length).toBe(0);

	const urls = nodes.flatMap((node) =>
		['href', 'src', 'xlink:href', 'srcdoc', 'action', 'formaction']
			.map((name) => node.getAttribute(name))
			.filter((value): value is string => value !== null)
	);
	expect(urls.filter((url) => !/^https?:\/\//.test(url))).toEqual([]);

	for (const link of document.body.querySelectorAll<HTMLElement>('a')) {
		if (!link.hasAttribute('href')) link.click();
	}
	await brokenImageSettled();
	expect(flagged.__xss).toBeUndefined();

	await expect.element(kept).toHaveAttribute('href', 'https://example.org/kept');
	await expect.element(kept).toHaveAttribute('target', '_blank');
	await expect.element(kept).toHaveAttribute('rel', 'noopener noreferrer');
}

describe('third-party HTML', () => {
	afterEach(() => {
		delete flagged.__xss;
		localStorage.clear();
	});

	it('renders provider HTML with nothing able to run and no images', async () => {
		await render(MetadataPanel, {
			props: { open: true, title: 'Blue Lines', notes: HOSTILE_HTML, onclose: () => {} }
		});

		await expectNothingRunnable();
		expect(document.body.querySelectorAll('img').length).toBe(0);
	});

	it('renders remote markdown with nothing able to run, keeping its images', async () => {
		await render(WhatsNewModal, {
			props: {
				currentVersion: 'v3.0.0',
				buildDate: null,
				releases: [
					{
						tag_name: 'v3.0.0',
						name: 'Three',
						body: HOSTILE_MARKDOWN,
						html_url: 'https://example.org/release',
						prerelease: false,
						published_at: '2026-10-01T00:00:00Z'
					}
				]
			}
		});

		await expect.element(page.getByRole('heading', { name: 'Kept heading' })).toBeVisible();
		await expectNothingRunnable();
		await expect
			.element(page.getByRole('img', { name: 'remote image' }))
			.toHaveAttribute('src', 'https://example.invalid/cover.png');
	});
});
