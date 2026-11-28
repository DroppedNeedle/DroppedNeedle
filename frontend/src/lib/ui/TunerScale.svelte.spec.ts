import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import TunerScale from './TunerScale.svelte';

const STATIONS = [
	{ id: 'far', distance: 0.12, name: 'Mariner by Cult of Luna, further' },
	{ id: 'near', distance: 0.02, name: 'The Ash by Julie Christmas, very close' },
	{ id: 'mid', distance: 0.07, name: 'King Park by La Dispute, close' }
];

describe('TunerScale', () => {
	it('sets the stations out as buttons in distance order, nearest first', async () => {
		await render(TunerScale, {
			label: 'Tracks that sound like Supernatural',
			stations: STATIONS,
			tuned: 'near',
			ontune: () => {},
			max: 0.15
		});
		const group = page.getByRole('group', { name: 'Tracks that sound like Supernatural' });
		const names = [...group.element().querySelectorAll('button')].map((b) =>
			b.getAttribute('aria-label')
		);
		expect(names).toEqual([
			'The Ash by Julie Christmas, very close',
			'King Park by La Dispute, close',
			'Mariner by Cult of Luna, further'
		]);
		await expect
			.element(page.getByRole('button', { name: 'The Ash by Julie Christmas, very close' }))
			.toHaveAttribute('aria-current', 'true');
	});

	it('tunes the station pressed', async () => {
		const ontune = vi.fn();
		await render(TunerScale, {
			label: 'Stations',
			stations: STATIONS,
			tuned: 'near',
			ontune,
			max: 0.15
		});
		await page.getByRole('button', { name: 'King Park by La Dispute, close' }).click();
		expect(ontune).toHaveBeenCalledWith('mid');
	});
});
