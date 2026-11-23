import { page } from 'vitest/browser';
import { createRawSnippet } from 'svelte';
import { describe, expect, it } from 'vitest';
import { cleanup, render } from 'vitest-browser-svelte';

import StatusChip, { type StateLook } from './StatusChip.svelte';

const WORDS: [StateLook, string][] = [
	['kept', 'Kept'],
	['later', 'Later'],
	['known', 'Know it'],
	['passed', 'Passed'],
	['waiting', 'Waiting'],
	['refused', 'Refused'],
	['missed', "Didn't arrive"]
];

describe('StatusChip', () => {
	it("says a pick's state in its own word, and keeps a caller's label", async () => {
		for (const [look, word] of WORDS) {
			cleanup();
			await render(StatusChip, { look });
			await expect.element(page.getByText(word, { exact: true })).toBeVisible();
		}

		// A tone chip still takes its words from the caller.
		cleanup();
		await render(StatusChip, {
			tone: 'success',
			children: createRawSnippet(() => ({ render: () => '<span>In library</span>' }))
		});
		await expect.element(page.getByText('In library')).toBeVisible();
	});
});
