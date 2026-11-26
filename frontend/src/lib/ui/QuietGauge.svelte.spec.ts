import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import QuietGauge from './QuietGauge.svelte';

describe('QuietGauge', () => {
	it('is one picture named by its sentence, from the first month to this one', async () => {
		await render(QuietGauge, {
			from: '2026-02',
			last: '2026-02',
			now: '2026-11',
			label: 'Last played in February, quiet for nine months'
		});
		const gauge = page.getByRole('img', {
			name: 'Last played in February, quiet for nine months'
		});
		await expect.element(gauge).toBeVisible();
		await expect.element(gauge).toHaveTextContent(/^\s*Feb\s*Nov\s*$/);
	});

	it('keeps the last twelve months of a longer history', async () => {
		await render(QuietGauge, {
			from: '2019-05',
			last: '2026-07',
			now: '2026-11',
			label: 'Last played in July'
		});
		const gauge = page.getByRole('img', { name: 'Last played in July' });
		await expect.element(gauge).toHaveTextContent(/^\s*Dec\s*Nov\s*$/);
	});
});
