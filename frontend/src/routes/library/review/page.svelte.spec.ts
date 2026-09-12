import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

vi.mock('$lib/components/library/LibraryReviewBrowser.svelte', () => {
	const Component = function () {};
	Component.prototype = {};
	return { default: Component };
});

import ReviewPage from './+page.svelte';

describe('identification review route', () => {
	it('uses the full-width library header with a back link', async () => {
		await render(ReviewPage);

		await expect
			.element(page.getByRole('heading', { name: 'Identification review' }))
			.toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Back to library' }))
			.toHaveAttribute('href', '/library');
	});
});
