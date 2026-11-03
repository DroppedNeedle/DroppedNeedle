import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import LibraryFilterMenu from './LibraryFilterMenu.svelte';

const counts = [
	{ format: 'mp3', album_count: 131 },
	{ format: 'flac', album_count: 96 }
];

describe('LibraryFilterMenu.svelte', () => {
	it('sets a minimum track count and says how many filters are on', async () => {
		const onMinTracks = vi.fn();
		render(LibraryFilterMenu, { props: { minTracks: 3, onMinTracks } });
		const button = page.getByRole('button', { name: /Filter/ });
		await expect.element(button).toHaveTextContent('Filter1');
		await button.click();
		await page.getByRole('button', { name: '5+' }).click();
		expect(onMinTracks).toHaveBeenLastCalledWith(5);
		await expect.element(page.getByText('Formats')).not.toBeInTheDocument();
	});

	it('marks formats with their album counts and switches to hiding them', async () => {
		const onFormats = vi.fn();
		render(LibraryFilterMenu, {
			props: {
				minTracks: 1,
				onMinTracks: () => {},
				formatCounts: counts,
				formats: ['flac'],
				formatMode: 'with',
				onFormats
			}
		});
		await page.getByRole('button', { name: /Filter/ }).click();
		await expect.element(page.getByRole('button', { name: /MP3\s*131/ })).toBeInTheDocument();
		await page.getByRole('button', { name: /MP3/ }).click();
		expect(onFormats).toHaveBeenLastCalledWith(['flac', 'mp3'], 'with');
		await page.getByRole('radio', { name: 'Without' }).click();
		expect(onFormats).toHaveBeenLastCalledWith(['flac'], 'without');
		await page.getByRole('button', { name: 'Reset' }).click();
		expect(onFormats).toHaveBeenLastCalledWith([], 'with');
	});
});
