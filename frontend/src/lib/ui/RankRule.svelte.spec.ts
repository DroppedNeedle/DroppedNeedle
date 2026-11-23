import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import RankRule from './RankRule.svelte';

describe('RankRule', () => {
	it('is one picture named by its sentence', async () => {
		await render(RankRule, { ticks: 100, at: 5, label: '5th closest of 100' });
		const rule = page.getByRole('img', { name: '5th closest of 100' });
		await expect.element(rule).toBeVisible();
	});
});
