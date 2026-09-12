import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import AlphabetJumpNav from './AlphabetJumpNav.svelte';

async function renderNav(letters: string[], props: Record<string, unknown> = {}) {
	for (const letter of letters) {
		const el = document.createElement('div');
		el.id = `letter-${letter}`;
		el.textContent = `Section ${letter}`;
		document.body.appendChild(el);
	}

	return await render(AlphabetJumpNav, {
		props: { letters, sectionIdPrefix: 'letter-', ...props }
	} as Parameters<typeof render<typeof AlphabetJumpNav>>[1]);
}

describe('AlphabetJumpNav.svelte', () => {
	it('renders letter buttons for A-Z and #', async () => {
		await renderNav(['A', 'B', 'Z']);
		// Both desktop and mobile navs render; use .first() to avoid strict-mode errors
		await expect.element(page.getByLabelText('Jump to A').first()).toBeInTheDocument();
		await expect.element(page.getByLabelText('Jump to Z').first()).toBeInTheDocument();
		await expect
			.element(page.getByLabelText('Jump to numbers and symbols').first())
			.toBeInTheDocument();
	});

	it('disables letters without content', async () => {
		await renderNav(['A', 'M']);
		await expect.element(page.getByLabelText('Jump to B').first()).toBeDisabled();
		await expect.element(page.getByLabelText('Jump to A').first()).not.toBeDisabled();
	});

	it('does not render when fewer than 2 letters are provided', async () => {
		await renderNav(['A']);
		const nav = page.getByRole('navigation');
		expect(nav.elements().length).toBe(0);
	});

	it('highlights the active letter with aria-current', async () => {
		await renderNav(['A', 'B', 'C']);
		await new Promise((r) => setTimeout(r, 200));
		const activeButtons = document.querySelectorAll('[aria-current="true"]');
		expect(activeButtons.length).toBeGreaterThanOrEqual(1);
	});

	it('calls onBeforeJump when a letter is clicked', async () => {
		const onBeforeJump = vi.fn();
		await renderNav(['A', 'B'], { onBeforeJump });

		await page.getByLabelText('Jump to B').first().click();
		expect(onBeforeJump).toHaveBeenCalledWith('B');
	});
});
