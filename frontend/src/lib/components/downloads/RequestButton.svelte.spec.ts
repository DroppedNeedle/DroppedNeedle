import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	configured: true,
	isAdmin: false,
	requestMutate: vi.fn(),
	requestPending: false
}));

vi.mock('$lib/stores/integration', () => ({
	integrationStore: {
		subscribe: (run: (v: { download_client: boolean }) => void) => {
			run({ download_client: h.configured });
			return () => {};
		}
	}
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: {
		get isAdmin() {
			return h.isAdmin;
		}
	}
}));

vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
	requestAlbum: () => ({
		mutate: h.requestMutate,
		get isPending() {
			return h.requestPending;
		}
	})
}));

import RequestButton from './RequestButton.svelte';

const base = { releaseGroupMbid: 'rg', title: 'OK Computer', artistName: 'Radiohead', year: 1997 };
async function renderButton(overrides: Record<string, unknown> = {}) {
	return await render(RequestButton, { props: { ...base, ...overrides } } as Parameters<
		typeof render<typeof RequestButton>
	>[1]);
}

describe('RequestButton.svelte', () => {
	beforeEach(() => {
		h.configured = true;
		h.isAdmin = false;
		h.requestMutate = vi.fn();
		h.requestPending = false;
	});

	it('shows "In Library" when the album is present', async () => {
		await renderButton({ inLibrary: true });
		await expect.element(page.getByText('In Library')).toBeVisible();
	});

	it('shows "Requested" when an active task exists', async () => {
		await renderButton({ requested: true });
		await expect.element(page.getByText('Requested')).toBeVisible();
	});

	it('shows "Configure Download Client" for admins when not configured', async () => {
		h.configured = false;
		h.isAdmin = true;
		await renderButton();
		await expect.element(page.getByText('Configure Download Client')).toBeVisible();
	});

	it('shows "Downloads Unavailable" for non-admins when not configured', async () => {
		h.configured = false;
		h.isAdmin = false;
		await renderButton();
		await expect.element(page.getByText('Downloads Unavailable')).toBeVisible();
	});

	it('shows "Request" and fires the mutation on click', async () => {
		await renderButton();
		await page.getByRole('button', { name: 'Request' }).click();
		expect(h.requestMutate).toHaveBeenCalled();
	});
});
