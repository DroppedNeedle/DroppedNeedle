import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it } from 'vitest';
import type { AlbumHistoryEntry } from '$lib/features/library/queries/LibraryAlbumHistoryQueries.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { apiError, fakeApi, type FakeRoutes } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

import AlbumHistory from './AlbumHistory.svelte';

const AT = Date.UTC(2026, 9, 12, 9, 30);
const HISTORY = 'GET /api/v3/library/albums/{album_id}/history';
const UNDO = 'POST /api/v3/library/folds/{fold_id}/undo';
const UNDO_ARTIST = 'POST /api/v3/library/artist-repairs/{repair_id}/undo';

function entry(overrides: Partial<AlbumHistoryEntry>): AlbumHistoryEntry {
	return {
		kind: 'tracks_continued',
		at: AT,
		by: null,
		track_count: 0,
		track_titles: [],
		other_album_title: null,
		undo_id: null,
		...overrides
	};
}

function signIn(role: 'admin' | 'trusted' | 'user') {
	authStore.setUser(
		toAuthUser({
			id: `${role}-1`,
			display_name: role,
			role,
			email: null,
			avatar_url: null,
			username: role,
			username_display: role
		})
	);
}

async function show(...items: AlbumHistoryEntry[]) {
	fakeApi({ [HISTORY]: { items } });
	await renderWithQuery(AlbumHistory, { albumId: 'album-1' });
}

const FIVE_OF_SIX = ['One', 'Two', 'Three', 'Four', 'Five'];

beforeEach(() => {
	localStorage.clear();
	signIn('user');
});

describe('AlbumHistory', () => {
	it('tells each kind of entry in one line, leaving titles out when none are sent', async () => {
		await show(
			entry({ kind: 'tracks_continued', track_count: 1, track_titles: ['Airbag'] }),
			entry({ kind: 'tracks_continued', track_count: 3 }),
			entry({ kind: 'tracks_continued', track_count: 6, track_titles: FIVE_OF_SIX }),
			entry({ kind: 'tracks_replaced', track_count: 1, track_titles: ['Lucky'] }),
			entry({ kind: 'tracks_replaced', track_count: 2, track_titles: ['Lucky', 'Karma'] }),
			entry({ kind: 'tracks_other_music', track_count: 1, track_titles: ['Airbag'] }),
			entry({
				kind: 'tracks_other_music',
				track_count: 3,
				track_titles: ['Airbag', 'Lucky', 'Karma']
			}),
			entry({ kind: 'tracks_new_copy', track_count: 1, track_titles: ['Airbag'] }),
			entry({
				kind: 'tracks_new_copy',
				track_count: 3,
				track_titles: ['Airbag', 'Lucky', 'Karma']
			}),
			entry({ kind: 'retag_joined', other_album_title: 'Old Name' }),
			entry({ kind: 'retag_joined', by: 'you', other_album_title: 'Old Name' }),
			entry({ kind: 'retag_joined', by: 'person', other_album_title: null }),
			entry({ kind: 'retag_half_left_alone', other_album_title: 'Old Name' }),
			entry({ kind: 'retag_dismissal_ended', other_album_title: 'Old Name' }),
			entry({ kind: 'leftover_joined', other_album_title: 'Old Name' }),
			entry({ kind: 'artist_from_tracks' }),
			entry({ kind: 'edition_changed' }),
			entry({ kind: 'match_withdrawn' }),
			entry({ kind: 'edition_chosen' }),
			entry({ kind: 'edition_change_undone', by: 'you' }),
			entry({ kind: 'edition_change_undone', by: 'person' }),
			entry({ kind: 'unfolded_from', other_album_title: 'Kept Name' }),
			entry({ kind: 'unfolded_from', by: 'you', other_album_title: 'Kept Name' }),
			entry({ kind: 'hand_back_given_up' })
		);
		await expect.element(page.getByRole('heading', { name: 'Album history' })).toBeInTheDocument();
		const lines = [
			'"Airbag" has a new file, and its plays, favorites and playlist entries moved to it.',
			'3 tracks have new files, and their plays, favorites and playlist entries moved to them.',
			'6 tracks have new files, and their plays, favorites and playlist entries moved to them: "One", "Two", "Three", "Four", "Five" and 1 more.',
			'"Lucky" was replaced by the edition\'s version, and its plays, favorites and playlist entries moved to the new file.',
			'2 tracks were replaced by the edition\'s version, and their plays, favorites and playlist entries moved to the new files: "Lucky", "Karma".',
			'"Airbag" was replaced by other music, which is now a new track. Its plays, favorites and playlist entries stayed with the old track, which shows as missing.',
			'3 tracks were replaced by other music, which came in as new tracks. Their plays, favorites and playlist entries stayed with the old tracks, which show as missing: "Airbag", "Lucky", "Karma".',
			'"Airbag" was replaced by a new copy of the same recording.',
			'3 tracks were replaced by new copies of the same recordings: "Airbag", "Lucky", "Karma".',
			'Old Name was joined into this album after its files were retagged outside DroppedNeedle.',
			'You merged Old Name into this album.',
			'An admin merged Another album into this album.',
			'Left unmanaged like Old Name, whose files were being retagged as this album.',
			'The tracks came back from Old Name, so "Keep them apart" no longer applies.',
			'Old Name, left over from an earlier version as a separate album, was folded into this album.',
			'The artist was taken from the tracks, replacing "Unknown Artist".',
			'DroppedNeedle moved this album to another edition.',
			"DroppedNeedle took this album's match off.",
			'DroppedNeedle picked an edition for this album.',
			"You undid DroppedNeedle's edition change.",
			"An admin undid DroppedNeedle's edition change.",
			'This album was brought back out of Kept Name, where it had been folded.',
			'You brought this album back out of Kept Name.',
			"Handing this album back stopped: its files couldn't be read."
		];
		for (const text of lines) {
			await expect.element(page.getByText(text, { exact: true })).toBeInTheDocument();
		}
		await expect.element(page.getByRole('button')).not.toBeInTheDocument();
	});

	it('renders titles as text', async () => {
		await show(entry({ kind: 'retag_joined', other_album_title: '<b>Bold</b>' }));
		await expect
			.element(
				page.getByText(
					'<b>Bold</b> was joined into this album after its files were retagged outside DroppedNeedle.',
					{ exact: true }
				)
			)
			.toBeInTheDocument();
	});
});

describe('undoing a fold or an artist change', () => {
	const folded = entry({
		kind: 'leftover_joined',
		other_album_title: 'Old Name',
		undo_id: 'fold-1'
	});
	const repaired = entry({ kind: 'artist_from_tracks', undo_id: 'repair-1' });

	it('undoes after a curator confirms, and says why a stale one cannot be undone', async () => {
		const cases = [
			{
				item: folded,
				route: UNDO,
				other: UNDO_ARTIST,
				params: { fold_id: 'fold-1' },
				confirm: ['Plays, favorites and playlist entries stay on this album.'],
				stale: "The albums changed after the join, so it can't be undone."
			},
			{
				item: repaired,
				route: UNDO_ARTIST,
				other: UNDO,
				params: { repair_id: 'repair-1' },
				confirm: [
					'Undo this artist change?',
					'The album goes back to "Unknown Artist", and this change is not made again.'
				],
				stale: "The album's artist changed after this, so it can't be undone."
			}
		] as const;
		for (const { item, route, other, params, confirm, stale } of cases) {
			signIn('trusted');
			const api = fakeApi({ [HISTORY]: { items: [item] }, [route]: undefined } as FakeRoutes);
			let view = await renderWithQuery(AlbumHistory, { albumId: 'album-1' });

			await page.getByRole('button', { name: 'Undo' }).click();
			const dialog = page.getByRole('dialog');
			for (const text of confirm) await expect.element(dialog.getByText(text)).toBeVisible();
			await dialog.getByRole('button', { name: 'Undo' }).click();

			await expect.element(page.getByRole('dialog')).not.toBeInTheDocument();
			expect(api.calls(route).map((call) => call.params)).toEqual([params]);
			expect(api.calls(other)).toEqual([]);
			view.unmount();

			signIn('admin');
			fakeApi({
				[HISTORY]: { items: [item] },
				[route]: apiError(409, 'STALE_REVISION', stale)
			} as FakeRoutes);
			view = await renderWithQuery(AlbumHistory, { albumId: 'album-1' });

			await page.getByRole('button', { name: 'Undo' }).click();
			await page.getByRole('dialog').getByRole('button', { name: 'Undo' }).click();

			await expect.element(page.getByRole('alert')).toHaveTextContent(stale);
			view.unmount();
		}
	});

	it('lists an undone fold as its own entry, with no Undo left on the fold', async () => {
		signIn('admin');
		await show(
			entry({ kind: 'fold_undone', by: 'you', other_album_title: 'Old Name' }),
			entry({ kind: 'leftover_joined', other_album_title: 'Old Name' })
		);
		await expect
			.element(
				page.getByText('You undid the fold: Old Name is its own album again.', { exact: true })
			)
			.toBeVisible();
		await expect.element(page.getByText(/was folded into this album/)).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Undo' })).not.toBeInTheDocument();
	});
});
