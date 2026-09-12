import { page } from '@vitest/browser/context';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import PlaylistMosaic from './PlaylistMosaic.svelte';

async function renderMosaic(props: Record<string, unknown> = {}) {
	return await render(PlaylistMosaic, {
		props
	} as Parameters<typeof render<typeof PlaylistMosaic>>[1]);
}

describe('PlaylistMosaic.svelte', () => {
	it('renders single img when customCoverUrl is provided', async () => {
		await renderMosaic({ customCoverUrl: 'https://example.com/custom.jpg' });
		const img = page.getByAltText('Playlist cover');
		await expect.element(img).toBeInTheDocument();
		expect(await img.element()).toHaveAttribute('src', 'https://example.com/custom.jpg');
	});

	it('custom cover overrides coverUrls even when URLs are provided', async () => {
		await renderMosaic({
			customCoverUrl: 'https://example.com/custom.jpg',
			coverUrls: ['a.jpg', 'b.jpg', 'c.jpg', 'd.jpg']
		});
		const images = page.getByAltText('Playlist cover');
		await expect.element(images).toBeInTheDocument();
		expect(await images.element()).toHaveAttribute('src', 'https://example.com/custom.jpg');
	});

	it('renders 4 img elements in grid for 4+ URLs', async () => {
		const { container } = await renderMosaic({
			coverUrls: ['a.jpg', 'b.jpg', 'c.jpg', 'd.jpg', 'e.jpg']
		});
		const elements = container.querySelectorAll('img');
		expect(elements).toHaveLength(4);
	});

	it('renders 3 img elements plus placeholder for 3 URLs', async () => {
		const { container } = await renderMosaic({ coverUrls: ['a.jpg', 'b.jpg', 'c.jpg'] });
		const elements = container.querySelectorAll('img');
		expect(elements).toHaveLength(3);
	});

	it('renders 2 img elements for 2 URLs', async () => {
		const { container } = await renderMosaic({ coverUrls: ['a.jpg', 'b.jpg'] });
		const elements = container.querySelectorAll('img');
		expect(elements).toHaveLength(2);
	});

	it('renders Music icon for 0 URLs', async () => {
		const { container } = await renderMosaic({ coverUrls: [] });
		const svg = container.querySelector('svg');
		expect(svg).toBeTruthy();
		const images = container.querySelectorAll('img');
		expect(images).toHaveLength(0);
	});

	it('applies default size and rounded props', async () => {
		const { container } = await renderMosaic();
		const wrapper = container.firstElementChild as HTMLElement;
		expect(wrapper.className).toContain('w-32');
		expect(wrapper.className).toContain('h-32');
		expect(wrapper.className).toContain('rounded-box');
		expect(wrapper.className).toContain('overflow-hidden');
	});

	it('applies custom size and rounded props', async () => {
		const { container } = await renderMosaic({ size: 'w-10 h-10', rounded: 'rounded-md' });
		const wrapper = container.firstElementChild as HTMLElement;
		expect(wrapper.className).toContain('w-10');
		expect(wrapper.className).toContain('h-10');
		expect(wrapper.className).toContain('rounded-md');
	});

	it('shows fallback placeholder when an image fails to load', async () => {
		expect.assertions(2);
		const { container } = await renderMosaic({ coverUrls: ['bad.jpg'] });
		const img = container.querySelector('img') as HTMLImageElement;
		expect(img).toBeTruthy();
		img.dispatchEvent(new Event('error'));
		await new Promise((r) => setTimeout(r, 50));
		const svg = container.querySelector('svg');
		expect(svg).toBeTruthy();
	});
});
