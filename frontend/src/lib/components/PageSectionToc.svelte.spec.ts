import { page } from '@vitest/browser/context';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: { isPlayerVisible: false }
}));

import PageSectionToc from './PageSectionToc.svelte';

const sections = [
	{ id: 'account', label: 'Account' },
	{ id: 'scrobbling', label: 'Scrobbling' },
	{ id: 'libraries', label: 'Your Libraries' }
];

let targets: HTMLElement[] = [];
let scrollSpy: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
	targets = sections.map((section) => {
		const target = document.createElement('section');
		target.id = section.id;
		document.body.append(target);
		return target;
	});
	scrollSpy = vi.spyOn(HTMLElement.prototype, 'scrollIntoView').mockImplementation(() => {});
});

afterEach(() => {
	for (const target of targets) target.remove();
	scrollSpy.mockRestore();
});

describe('PageSectionToc', () => {
	it('scrolls to a section and marks its link as current', async () => {
		render(PageSectionToc, { sections });
		const navigation = page.getByRole('navigation', { name: 'Page sections' });
		const scrobblingLink = navigation.getByRole('link', { name: 'Scrobbling' });

		await expect.element(navigation.getByText('On this page')).toBeInTheDocument();
		await scrobblingLink.click();

		expect(scrollSpy).toHaveBeenCalledWith({ behavior: 'smooth', block: 'start' });
		await expect.element(scrobblingLink).toHaveAttribute('aria-current', 'true');
	});
});
