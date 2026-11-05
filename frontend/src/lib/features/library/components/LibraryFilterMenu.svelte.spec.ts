import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import LibraryFilterMenu from './LibraryFilterMenu.svelte';

describe('LibraryFilterMenu.svelte', () => {
	it('shows only the categories the list hands a handler for', async () => {
		render(LibraryFilterMenu, { onRating: vi.fn() });
		await page.getByRole('button', { name: 'Filter' }).click();

		await expect.element(page.getByText('Your rating')).toBeVisible();
		expect(page.getByText('Formats').elements()).toHaveLength(0);
		expect(page.getByRole('radiogroup', { name: 'Track filter mode' }).elements()).toHaveLength(0);
	});

	it('turns each category around with its own direction', async () => {
		const onMinTracks = vi.fn();
		const onFormats = vi.fn();
		const onRating = vi.fn();
		render(LibraryFilterMenu, {
			minTracks: 3,
			onMinTracks,
			formatCounts: [{ format: 'flac', album_count: 4 }],
			formats: ['flac'],
			onFormats,
			rating: 4,
			onRating
		});
		// On a list with formats the button reads as the format select.
		await page.getByRole('button', { name: /FLAC/ }).click();

		await page
			.getByRole('radiogroup', { name: 'Track filter mode' })
			.getByRole('radio', { name: 'Fewer than' })
			.click();
		expect(onMinTracks).toHaveBeenLastCalledWith(3, 'below');
		await page
			.getByRole('radiogroup', { name: 'Format filter mode' })
			.getByRole('radio', { name: 'Without' })
			.click();
		expect(onFormats).toHaveBeenLastCalledWith(['flac'], 'without');
		await page
			.getByRole('radiogroup', { name: 'Rating filter mode' })
			.getByRole('radio', { name: 'At most' })
			.click();
		expect(onRating).toHaveBeenLastCalledWith(4, 'atMost');
		await page.getByRole('button', { name: '2 stars' }).click();
		expect(onRating).toHaveBeenLastCalledWith(2, 'atLeast');
	});

	it('counts the active categories and resets them all', async () => {
		const onMinTracks = vi.fn();
		const onFormats = vi.fn();
		const onRating = vi.fn();
		render(LibraryFilterMenu, {
			minTracks: 2,
			tracksMode: 'below',
			onMinTracks,
			formatCounts: [],
			formats: [],
			onFormats,
			rating: 5,
			ratingMode: 'atMost',
			onRating
		});
		const button = page.getByRole('button', { name: /All formats/ });
		await expect.element(button).toHaveTextContent('2');
		await button.click();

		await page.getByRole('button', { name: 'Reset' }).click();
		expect(onMinTracks).toHaveBeenLastCalledWith(1, 'atLeast');
		expect(onFormats).toHaveBeenLastCalledWith([], 'with');
		expect(onRating).toHaveBeenLastCalledWith(0, 'atLeast');
	});
});
