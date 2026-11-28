import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import Meter from './Meter.svelte';

describe('Meter', () => {
	it('is one picture named by its sentence, with words and no numbers', async () => {
		await render(Meter, { label: 'The Ash sounds very close', value: 0.9, caption: 'How close' });
		const meter = page.getByRole('img', { name: 'The Ash sounds very close' });
		await expect.element(meter).toBeVisible();
		await expect.element(meter).toHaveTextContent(/^\s*Far\s*Close\s*$/);
		await expect.element(page.getByText('How close')).toBeVisible();
	});

	it('swings its needle further towards Close for a closer value, inside its sweep', async () => {
		const angle = async (value: number, label: string) => {
			await render(Meter, { label, value });
			const face = page.getByRole('img', { name: label }).element();
			const needle = face.querySelector<HTMLElement>('.dn-meter__needle');
			return parseFloat(needle?.style.rotate ?? 'NaN');
		};
		const far = await angle(0.2, 'Far one');
		const near = await angle(0.9, 'Near one');
		expect(near).toBeGreaterThan(far);
		for (const at of [far, near]) {
			expect(at).toBeGreaterThanOrEqual(-55);
			expect(at).toBeLessThanOrEqual(55);
		}
	});
});
