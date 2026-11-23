import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { cleanup, render } from 'vitest-browser-svelte';

import Sticker from './Sticker.svelte';
import type { StateLook } from './StatusChip.svelte';

const WORDS: [StateLook, string][] = [
	['kept', 'Kept'],
	['later', 'Later'],
	['known', 'Know it'],
	['passed', 'Passed'],
	['waiting', 'Waiting'],
	['refused', 'Refused'],
	['missed', "Didn't arrive"]
];

describe('Sticker', () => {
	it('says each state in its word, never in colour alone', async () => {
		for (const [state, word] of WORDS) {
			cleanup();
			await render(Sticker, { state, fresh: true });
			await expect.element(page.getByText(word, { exact: true })).toBeVisible();
		}
	});
});
