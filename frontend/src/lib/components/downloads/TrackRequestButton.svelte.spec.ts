import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({ mutate: vi.fn(), pending: false }));

vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
	requestTrack: () => ({
		mutate: h.mutate,
		get isPending() {
			return h.pending;
		}
	})
}));

import TrackRequestButton from './TrackRequestButton.svelte';

const base = {
	recordingMbid: 'rec',
	trackTitle: 'Airbag',
	artistName: 'Radiohead',
	albumMbid: 'rg'
};
async function renderButton(overrides: Record<string, unknown> = {}) {
	return await render(TrackRequestButton, { props: { ...base, ...overrides } } as Parameters<
		typeof render<typeof TrackRequestButton>
	>[1]);
}

describe('TrackRequestButton.svelte', () => {
	beforeEach(() => {
		h.mutate = vi.fn();
		h.pending = false;
	});

	it('renders an accessible per-track request button', async () => {
		await renderButton();
		await expect.element(page.getByRole('button', { name: 'Request this track' })).toBeVisible();
	});

	it('fires requestTrack on click', async () => {
		await renderButton();
		await page.getByRole('button', { name: 'Request this track' }).click();
		expect(h.mutate).toHaveBeenCalled();
	});

	it('sends the displayed selected edition as release_id in the exact-track mutation', async () => {
		await renderButton({ releaseMbid: 'release-20' });
		await page.getByRole('button', { name: 'Request this track' }).click();
		expect(h.mutate).toHaveBeenCalledTimes(1);
		expect(h.mutate.mock.calls[0][0]).toMatchObject({
			recording_mbid: 'rec',
			release_group_mbid: 'rg',
			release_id: 'release-20'
		});
	});

	it('keeps release_id null when no edition is selected', async () => {
		await renderButton();
		await page.getByRole('button', { name: 'Request this track' }).click();
		expect(h.mutate).toHaveBeenCalledTimes(1);
		expect(h.mutate.mock.calls[0][0].release_id).toBeNull();
	});
});
