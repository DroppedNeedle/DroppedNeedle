import { page } from '@vitest/browser/context';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import LibraryFormatBadge from './LibraryFormatBadge.svelte';

describe('LibraryFormatBadge (F-PERF-10 display policy)', () => {
	it('renders MIXED as a neutral ghost badge without lossless or MP3 classes', async () => {
		await render(LibraryFormatBadge, {
			props: { format: 'mixed' }
		} as unknown as Parameters<typeof render>[1]);

		const badge = page.getByText('MIXED');
		await expect.element(badge).toBeVisible();
		await expect.element(badge).toHaveClass(/badge-ghost/);
		const classList = await badge.element().getAttribute('class');
		expect(classList ?? '').not.toMatch(/badge-success/); // no lossless colour
		expect(classList ?? '').not.toMatch(/badge-info/); // no MP3 colour
	});

	it('keeps the existing homogeneous labels unchanged', async () => {
		const { unmount } = await render(LibraryFormatBadge, {
			props: { format: 'flac' }
		} as unknown as Parameters<typeof render>[1]);
		await expect.element(page.getByText('FLAC')).toBeVisible();
		await expect.element(page.getByText('FLAC')).toHaveClass(/badge-success/);
		await unmount();

		await render(LibraryFormatBadge, {
			props: { format: 'mp3' }
		} as unknown as Parameters<typeof render>[1]);
		await expect.element(page.getByText('MP3')).toBeVisible();
		await expect.element(page.getByText('MP3')).toHaveClass(/badge-info/);
	});
});
