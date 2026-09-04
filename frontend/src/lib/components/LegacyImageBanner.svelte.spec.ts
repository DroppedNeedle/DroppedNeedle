import { page } from '@vitest/browser/context';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	query: { data: undefined as { version: string } | undefined }
}));

vi.mock('$lib/queries/VersionQuery.svelte', () => ({
	getVersionQuery: () => h.query
}));

import LegacyImageBanner from './LegacyImageBanner.svelte';

describe('LegacyImageBanner.svelte', () => {
	let unmount: (() => void) | undefined;
	beforeEach(() => {
		h.query.data = undefined;
	});
	afterEach(() => {
		unmount?.();
		unmount = undefined;
	});

	it('is hidden while version is loading', async () => {
		expect.assertions(1);
		({ unmount } = render(LegacyImageBanner));
		await expect.element(page.getByRole('alert')).not.toBeInTheDocument();
	});

	it('is hidden on v2.10.2', async () => {
		expect.assertions(1);
		h.query.data = { version: 'v2.10.2' };
		({ unmount } = render(LegacyImageBanner));
		await expect.element(page.getByRole('alert')).not.toBeInTheDocument();
	});

	it('is hidden on dev', async () => {
		expect.assertions(1);
		h.query.data = { version: 'dev' };
		({ unmount } = render(LegacyImageBanner));
		await expect.element(page.getByRole('alert')).not.toBeInTheDocument();
	});

	it('is visible on v2.11.0 with both image names', async () => {
		expect.assertions(3);
		h.query.data = { version: 'v2.11.0' };
		({ unmount } = render(LegacyImageBanner));
		await expect.element(page.getByText('Old image names are retired')).toBeVisible();
		await expect
			.element(page.getByText('droppedneedle/droppedneedle', { exact: true }))
			.toBeVisible();
		await expect
			.element(page.getByText('ghcr.io/droppedneedle/droppedneedle', { exact: true }))
			.toBeVisible();
	});

	it('is visible on v2.11.1', async () => {
		expect.assertions(1);
		h.query.data = { version: 'v2.11.1' };
		({ unmount } = render(LegacyImageBanner));
		await expect.element(page.getByText('Old image names are retired')).toBeVisible();
	});

	it('dismiss button hides it for the session', async () => {
		expect.assertions(3);
		h.query.data = { version: 'v2.11.0' };
		({ unmount } = render(LegacyImageBanner));
		await expect.element(page.getByRole('alert')).toBeVisible();
		// Playwright mouse clicks miss small edge-anchored buttons in the scaled
		// test iframe (in-page elementsFromPoint shows a clean hit path, yet no
		// event arrives), so dispatch in-page. This still exercises the real
		// Svelte onclick handler, session state, and DOM removal.
		const dismissButton = document.querySelector('button[aria-label="Dismiss"]');
		expect(dismissButton).not.toBeNull();
		(dismissButton as HTMLButtonElement).click();
		await expect.element(page.getByRole('alert')).not.toBeInTheDocument();
	});
});
