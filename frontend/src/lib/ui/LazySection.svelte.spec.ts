import { page } from 'vitest/browser';
import { createRawSnippet } from 'svelte';
import { describe, expect, it, onTestFinished, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import LazySection from './LazySection.svelte';

const shelf = createRawSnippet(() => ({ render: () => '<p>More by Lumen</p>' }));

const frame = () => new Promise((resolve) => requestAnimationFrame(resolve));

// Three screens of page above the section.
function pageAbove(): HTMLDivElement {
	const spacer = document.createElement('div');
	spacer.style.height = `${window.innerHeight * 3}px`;
	document.body.prepend(spacer);
	onTestFinished(() => {
		spacer.remove();
		window.scrollTo(0, 0);
	});
	return spacer;
}

describe('LazySection', () => {
	it('mounts its content once it comes within a screen of view, not before', async () => {
		const spacer = pageAbove();
		window.scrollTo(0, 0);
		const onshow = vi.fn();
		await render(LazySection, { children: shelf, onshow });
		await frame();
		await frame();

		expect(page.getByText('More by Lumen').query()).toBeNull();
		expect(onshow).not.toHaveBeenCalled();

		// Half a screen below the bottom of the view.
		window.scrollTo(0, spacer.offsetHeight - window.innerHeight * 1.5);
		// The observer reports on a later frame, which can take a while on a busy host.
		await expect.poll(() => onshow.mock.calls.length, { timeout: 5000, interval: 200 }).toBe(1);
		await expect.element(page.getByText('More by Lumen')).toBeInTheDocument();
	});
});
