import { page } from 'vitest/browser';
import { createRawSnippet } from 'svelte';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import Panel from './Panel.svelte';

const body = createRawSnippet(() => ({ render: () => '<p>Drop the needle</p>' }));

describe('Panel', () => {
	it("carries its caller's name and handlers to the panel itself", async () => {
		const ondrop = vi.fn((event: DragEvent) => event.preventDefault());
		await render(Panel, {
			as: 'section',
			look: 'metal',
			'aria-label': 'Now playing turntable',
			ondrop,
			children: body
		});
		const panel = page.getByRole('region', { name: 'Now playing turntable' });
		await expect.element(panel.getByText('Drop the needle')).toBeVisible();

		// A record dropped anywhere on the deck reaches the deck's handler.
		panel
			.getByText('Drop the needle')
			.element()
			.dispatchEvent(new DragEvent('drop', { bubbles: true, cancelable: true }));
		expect(ondrop).toHaveBeenCalledOnce();
	});
});
