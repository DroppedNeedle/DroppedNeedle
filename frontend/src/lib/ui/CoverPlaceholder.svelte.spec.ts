import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import CoverPlaceholder from './CoverPlaceholder.svelte';

const ALBUM_ID = '5b11f4ce-a62d-471e-81fc-a69a8278c7da';
const OTHER_ID = 'a7c3e2d0-8f41-4b6e-9c2a-5d1e0f3b7a94';
const THIRD_ID = '0e9d6f2b-4c1a-4f7e-8b3d-2a5c9e1f6d70';

const TOKEN_GRADIENT =
	/^linear-gradient\(\d+deg, var\(--color-[a-z0-9-]+\), var\(--color-[a-z0-9-]+\)\)$/;

async function paint(props: { id?: string | null; name?: string; kind?: 'album' | 'artist' }) {
	const view = await render(CoverPlaceholder, props);
	const element = page.getByTestId('cover-placeholder').element() as HTMLElement;
	const background = element.style.backgroundImage;
	await view.unmount();
	return background;
}

describe('CoverPlaceholder', () => {
	it('gives the same id the same gradient', async () => {
		const album = await paint({ id: ALBUM_ID, kind: 'album' });
		const artist = await paint({ id: ALBUM_ID.toUpperCase(), kind: 'artist' });

		expect(album).not.toBe('');
		expect(artist).toBe(album);
	});

	it('gives different ids different gradients', async () => {
		const gradients = new Set([
			await paint({ id: ALBUM_ID }),
			await paint({ id: OTHER_ID }),
			await paint({ id: THIRD_ID })
		]);

		expect(gradients.size).toBe(3);
	});

	it('draws its colours from theme tokens only', async () => {
		for (const props of [
			{ id: ALBUM_ID },
			{ id: OTHER_ID, kind: 'artist' as const },
			{ name: 'Kind of Blue' },
			{}
		]) {
			expect(await paint(props)).toMatch(TOKEN_GRADIENT);
		}
	});
});
