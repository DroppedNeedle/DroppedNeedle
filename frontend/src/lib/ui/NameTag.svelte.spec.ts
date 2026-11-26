import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import NameTag from './NameTag.svelte';

describe('NameTag', () => {
	it('names the thing, and its remove button is a 44 px target that removes it', async () => {
		const onremove = vi.fn();
		await render(NameTag, { name: 'Circa Survive', onremove });

		await expect.element(page.getByText('Circa Survive')).toBeVisible();
		const remove = page.getByRole('button', { name: 'Remove Circa Survive' });
		const box = (remove.element() as HTMLElement).getBoundingClientRect();
		expect(box.width).toBeGreaterThanOrEqual(44);
		expect(box.height).toBeGreaterThanOrEqual(44);
		await remove.click();
		expect(onremove).toHaveBeenCalledOnce();
	});
});
