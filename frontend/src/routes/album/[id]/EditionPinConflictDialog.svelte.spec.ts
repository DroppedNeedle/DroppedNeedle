import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { LibraryAlbumSummary } from '$lib/types';

const h = vi.hoisted(() => ({
	setLocalPin: vi.fn(),
	clearLocalPin: vi.fn(),
	toast: vi.fn()
}));

vi.mock('$lib/queries/albums/EditionQueries.svelte', () => ({
	setLocalAlbumEditionPin: () => ({ mutateAsync: h.setLocalPin, isPending: false }),
	clearLocalAlbumEditionPin: () => ({ mutateAsync: h.clearLocalPin, isPending: false })
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-1' }, isTrusted: true },
	LAST_USER_ID_KEY: 'test:last-user'
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: (...args: unknown[]) => h.toast(...args) }
}));

import EditionPinConflictDialog from './EditionPinConflictDialog.svelte';

function localCopy(id: string, title: string): LibraryAlbumSummary {
	return {
		id,
		title,
		artist_name: 'Local Artist',
		artist_id: 'local-artist-1',
		musicbrainz_release_group_id: 'rg-1',
		musicbrainz_release_id: null,
		musicbrainz_artist_id: null,
		album_identity_state: 'release_linked',
		track_count: 20,
		total_duration_seconds: 3900,
		total_size_bytes: 1,
		format: 'flac',
		year: 2008,
		is_compilation: false,
		cover_available: false,
		date_added: 1,
		sort_name: null,
		original_release_date: null,
		contribution_id: null,
		contribution_state: null
	};
}

function openDialog(): HTMLDialogElement {
	const dialog = document.querySelector('dialog');
	if (!(dialog instanceof HTMLDialogElement)) throw new Error('conflict dialog did not render');
	dialog.showModal();
	return dialog;
}

beforeEach(() => {
	vi.clearAllMocks();
	h.setLocalPin.mockResolvedValue(undefined);
	h.clearLocalPin.mockResolvedValue(undefined);
});

describe('EditionPinConflictDialog', () => {
	it('pins the chosen copy through the local URL, then refreshes and closes', async () => {
		const onrefresh = vi.fn();
		const onclose = vi.fn();
		render(EditionPinConflictDialog, {
			props: {
				releaseMbid: 'release-11',
				localCopies: [
					localCopy('local-album-1', 'Avalon'),
					localCopy('local-album-2', 'Avalon Remaster')
				],
				onrefresh,
				onclose
			}
		} as unknown as Parameters<typeof render>[1]);

		const dialog = openDialog();
		await expect
			.element(page.getByRole('heading', { name: 'Which copy should this edition apply to?' }))
			.toBeVisible();

		await page.getByRole('button', { name: 'Avalon Local Artist · 20 tracks' }).click();
		await vi.waitFor(() => {
			expect(h.setLocalPin).toHaveBeenCalledWith({
				userId: 'user-1',
				localId: 'local-album-1',
				rgMbid: 'rg-1',
				releaseMbid: 'release-11'
			});
			expect(onrefresh).toHaveBeenCalledOnce();
			expect(onclose).toHaveBeenCalledOnce();
		});
		await vi.waitFor(() => {
			expect(dialog.open).toBe(false);
		});
	});

	it('clears the chosen copy back to Automatic when the intent is null', async () => {
		const onrefresh = vi.fn();
		const onclose = vi.fn();
		render(EditionPinConflictDialog, {
			props: {
				releaseMbid: null,
				localCopies: [localCopy('local-album-1', 'Avalon')],
				onrefresh,
				onclose
			}
		} as unknown as Parameters<typeof render>[1]);

		openDialog();
		await page.getByRole('button', { name: /Avalon.*20 tracks/ }).click();
		await vi.waitFor(() => {
			expect(h.clearLocalPin).toHaveBeenCalledWith({
				userId: 'user-1',
				localId: 'local-album-1',
				rgMbid: 'rg-1'
			});
			expect(onrefresh).toHaveBeenCalledOnce();
		});
	});
});
