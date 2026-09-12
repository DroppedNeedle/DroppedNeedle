import { page, userEvent } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const mutateAsync = vi.fn();

vi.mock('$lib/queries/library/LibraryMutations.svelte', () => ({
	removeLibraryAlbum: () => ({ mutateAsync, isPending: false })
}));

import DeleteAlbumModal from './DeleteAlbumModal.svelte';

async function renderModal(ondeleted = vi.fn(), onclose = vi.fn()) {
	return {
		ondeleted,
		onclose,
		...(await render(DeleteAlbumModal, {
			props: {
				albumTitle: 'Blue Lines',
				artistName: 'Massive Attack',
				musicbrainzId: 'rg-1',
				ondeleted,
				onclose
			}
		} as unknown as Parameters<typeof render<typeof DeleteAlbumModal>>[1]))
	};
}

describe('DeleteAlbumModal', () => {
	beforeEach(() => {
		mutateAsync.mockReset();
		mutateAsync.mockResolvedValue({ success: true });
	});

	it('describes only the selected album file deletion', async () => {
		await renderModal();

		await expect.element(page.getByRole('heading', { name: 'Remove Album' })).toBeVisible();
		await expect.element(page.getByText(/Blue Lines/)).toBeVisible();
		await expect.element(page.getByText(/permanently deleted from disk/)).toBeVisible();
		await expect.element(page.getByText(/also remove/i)).not.toBeInTheDocument();
		await expect.element(page.getByText(/Checking artist impact/i)).not.toBeInTheDocument();
	});

	it('waits for removal before reporting success', async () => {
		const { ondeleted } = await renderModal();

		await page.getByRole('button', { name: 'Remove' }).click();

		expect(mutateAsync).toHaveBeenCalledWith({ mbid: 'rg-1', stopWanted: true });
		await vi.waitFor(() => expect(ondeleted).toHaveBeenCalledOnce());
	});

	it('lets the administrator keep the Wanted watch', async () => {
		await renderModal();
		const checkbox = page.getByRole('checkbox', { name: /Stop the Wanted watcher/i });
		await expect.element(checkbox).toBeChecked();

		await checkbox.click();
		await page.getByRole('button', { name: 'Remove' }).click();

		expect(mutateAsync).toHaveBeenCalledWith({ mbid: 'rg-1', stopWanted: false });
	});

	it('keeps the confirmation open when removal fails', async () => {
		mutateAsync.mockRejectedValueOnce(new Error("Couldn't remove this album"));
		const { ondeleted } = await renderModal();

		await page.getByRole('button', { name: 'Remove' }).click();

		await expect.element(page.getByText("Couldn't remove this album")).toBeVisible();
		await expect.element(page.getByRole('heading', { name: 'Remove Album' })).toBeVisible();
		expect(ondeleted).not.toHaveBeenCalled();
	});

	it('closes with the keyboard without removing the album', async () => {
		const { onclose } = await renderModal();

		await userEvent.keyboard('{Escape}');

		await vi.waitFor(() => expect(onclose).toHaveBeenCalledOnce());
		expect(mutateAsync).not.toHaveBeenCalled();
	});
});
