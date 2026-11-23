import { page } from 'vitest/browser';
import { createRawSnippet } from 'svelte';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import Tape from './Tape.svelte';

const words = (text: string) => createRawSnippet(() => ({ render: () => `<span>${text}</span>` }));

describe('Tape', () => {
	it('an amber tape always carries its warning sign, never the colour alone', async () => {
		await render(Tape, { look: 'warn', children: words('ListenBrainz has not answered') });
		await expect.element(page.getByText('ListenBrainz has not answered')).toBeVisible();
		await expect.element(page.getByRole('img', { name: 'Warning' })).toBeVisible();
	});
});
