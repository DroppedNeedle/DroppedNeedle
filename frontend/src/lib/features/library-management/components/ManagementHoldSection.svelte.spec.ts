import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { AlbumDetailView } from '$lib/types';

const album = {
	id: 'album-1',
	title: 'Local Signals',
	artist_name: 'Signal Artist',
	release_group_mbid: null,
	release_mbid: null,
	management_hold: null
} as unknown as AlbumDetailView;

const h = vi.hoisted(() => ({
	handBack: vi.fn(),
	reenable: vi.fn(),
	history: [] as Array<{ kind: string; at: number }>
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'admin-1' } },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));
vi.mock('$lib/features/library/queries/LibraryCatalogMutations.svelte', () => ({
	createReenableAlbumManagementMutation: () => ({ mutateAsync: h.reenable, isPending: false })
}));
vi.mock('$lib/features/library/queries/EditionQueries.svelte', () => ({
	getAlbumEditionsQuery: () => ({
		data: { items: [{ release_mbid: 'release-1', title: 'Local Signals (Deluxe)' }] }
	})
}));
vi.mock('$lib/features/library/queries/LibraryAlbumHistoryQueries.svelte', () => ({
	getAlbumHistoryQuery: () => ({
		get data() {
			return { items: h.history };
		}
	})
}));
vi.mock('$lib/features/library-management/queries/LibraryManagementMutations.svelte', () => ({
	createHandBackLibraryAlbumMutation: () => ({
		mutateAsync: h.handBack,
		isPending: false,
		error: null
	}),
	handBackRefusalMessage: () => null
}));

import ManagementHoldSection from './ManagementHoldSection.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.history = [];
	h.handBack.mockResolvedValue({ handed_back: true });
	h.reenable.mockResolvedValue({});
});

describe('an album left unmanaged', () => {
	const heldAt = Date.UTC(2026, 9, 1, 12);
	const renderHeld = (
		hold: NonNullable<AlbumDetailView['management_hold']>,
		overrides: Partial<AlbumDetailView> = {}
	) =>
		render(ManagementHoldSection, {
			props: { album: { ...album, ...overrides, management_hold: hold } }
		});

	it.each([
		['files_changed', 'These files were changed outside DroppedNeedle'],
		['retag_followed', 'This album was retagged outside DroppedNeedle'],
		['run_undone', "You undid DroppedNeedle's changes to this album"],
		['replaced_hands_off', 'These files may be your retag of another album'],
		['your_choice', 'You chose to leave this album unmanaged']
	])('says what %s means and offers one button', async (kind, heading) => {
		await renderHeld({ kind, held_at: heldAt, handing_back: false, exclusion_revision: 3 });

		const section = page.getByRole('region', { name: 'Left unmanaged' });
		await expect.element(section.getByRole('heading', { name: heading })).toBeVisible();
		await expect.element(section.getByRole('button')).toHaveTextContent('Manage it again');
	});

	it('hands a stepped-back album back, saying beside the button what that overwrites', async () => {
		await renderHeld(
			{ kind: 'files_changed', held_at: heldAt, handing_back: false },
			{ release_group_mbid: 'rg-1', release_mbid: 'release-1' }
		);

		const section = page.getByRole('region', { name: 'Left unmanaged' });
		await expect
			.element(section)
			.toHaveTextContent(
				'DroppedNeedle will retag and rename these files from Local Signals (Deluxe). Changes made outside DroppedNeedle are overwritten.'
			);
		await section.getByRole('button', { name: 'Manage it again' }).click();

		expect(h.handBack).toHaveBeenCalledWith({ albumId: 'album-1' });
		expect(h.reenable).not.toHaveBeenCalled();
	});

	it("lifts the person's own choice with the exclusion's revision", async () => {
		await renderHeld({
			kind: 'your_choice',
			held_at: heldAt,
			handing_back: false,
			exclusion_revision: 7
		});

		await page.getByRole('button', { name: 'Manage it again' }).click();

		expect(h.reenable).toHaveBeenCalledWith({ albumId: 'album-1', expectedRevision: 7 });
		expect(h.handBack).not.toHaveBeenCalled();
	});

	it('says a hand-back stopped since the album was held, and offers it again', async () => {
		h.history = [
			{ kind: 'hand_back_given_up', at: heldAt + 1000 },
			{ kind: 'hand_back_given_up', at: heldAt - 1000 }
		];
		await renderHeld({ kind: 'files_changed', held_at: heldAt, handing_back: false });

		const section = page.getByRole('region', { name: 'Left unmanaged' });
		await expect
			.element(section.getByText("Handing it back stopped: the files couldn't be read."))
			.toBeVisible();
		await expect.element(section.getByRole('button')).toHaveTextContent('Manage it again');
	});
});
