import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it } from 'vitest';
import type { AlbumHistoryEntry } from '$lib/queries/library/LibraryAlbumHistoryQueries.svelte';
import { toAuthUser } from '$lib/queries/auth/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { apiError, fakeApi } from '$lib/test/api';
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
	it('tells each kind of entry in one line', async () => {
		await show(
			entry({ kind: 'tracks_continued', track_count: 1, track_titles: ['Airbag'] }),
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
			entry({ kind: 'leftover_joined', other_album_title: 'Old Name' }),
			entry({ kind: 'artist_from_tracks' })
		);
		await expect.element(page.getByRole('heading', { name: 'Album history' })).toBeInTheDocument();
		const lines = [
			'"Airbag" has a new file, and its plays, favorites and playlist entries moved to it.',
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
			'Old Name, left over from an earlier version as a separate album, was folded into this album.',
			'The artist was taken from the tracks, replacing "Unknown Artist".'
		];
		for (const text of lines) {
			await expect.element(page.getByText(text, { exact: true })).toBeInTheDocument();
		}
		await expect.element(page.getByRole('button')).not.toBeInTheDocument();
	});

	it('leaves the titles out when none are sent', async () => {
		await show(entry({ kind: 'tracks_continued', track_count: 3 }));
		await expect
			.element(
				page.getByText(
					'3 tracks have new files, and their plays, favorites and playlist entries moved to them.',
					{ exact: true }
				)
			)
			.toBeInTheDocument();
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

	it('dates each line with a machine-readable time', async () => {
		await show(entry({ kind: 'leftover_joined', other_album_title: 'Old Name' }));
		const time = page.getByTitle(new Date(AT).toLocaleString());
		await expect.element(time).toHaveAttribute('datetime', new Date(AT).toISOString());
	});

	it('shows nothing, not even the heading, when there is nothing to tell', async () => {
		await show();
		await expect
			.element(page.getByRole('heading', { name: 'Album history' }))
			.not.toBeInTheDocument();
	});
});

describe('undoing a fold', () => {
	const folded = entry({
		kind: 'leftover_joined',
		other_album_title: 'Old Name',
		undo_id: 'fold-1'
	});

	it('undoes the fold after a curator confirms', async () => {
		signIn('trusted');
		const api = fakeApi({ [HISTORY]: { items: [folded] }, [UNDO]: undefined });
		await renderWithQuery(AlbumHistory, { albumId: 'album-1' });

		await page.getByRole('button', { name: 'Undo' }).click();
		const dialog = page.getByRole('dialog');
		await expect
			.element(dialog.getByText('Plays, favorites and playlist entries stay on this album.'))
			.toBeVisible();
		await dialog.getByRole('button', { name: 'Undo' }).click();

		await expect.element(page.getByRole('dialog')).not.toBeInTheDocument();
		expect(api.calls(UNDO).map((call) => call.params)).toEqual([{ fold_id: 'fold-1' }]);
	});

	it('says why a stale fold cannot be undone', async () => {
		signIn('admin');
		fakeApi({
			[HISTORY]: { items: [folded] },
			[UNDO]: apiError(
				409,
				'STALE_REVISION',
				"The albums changed after the join, so it can't be undone."
			)
		});
		await renderWithQuery(AlbumHistory, { albumId: 'album-1' });

		await page.getByRole('button', { name: 'Undo' }).click();
		await page.getByRole('dialog').getByRole('button', { name: 'Undo' }).click();

		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent("The albums changed after the join, so it can't be undone.");
	});

	it('offers no Undo to a listener', async () => {
		await show(folded);
		await expect.element(page.getByText(/was folded into this album/)).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Undo' })).not.toBeInTheDocument();
	});
});

describe('undoing an artist change', () => {
	const repaired = entry({ kind: 'artist_from_tracks', undo_id: 'repair-1' });

	it('undoes the artist change after a curator confirms', async () => {
		signIn('trusted');
		const api = fakeApi({ [HISTORY]: { items: [repaired] }, [UNDO_ARTIST]: undefined });
		await renderWithQuery(AlbumHistory, { albumId: 'album-1' });

		await page.getByRole('button', { name: 'Undo' }).click();
		const dialog = page.getByRole('dialog');
		await expect.element(dialog.getByText('Undo this artist change?')).toBeVisible();
		await expect
			.element(
				dialog.getByText(
					'The album goes back to "Unknown Artist", and this change is not made again.'
				)
			)
			.toBeVisible();
		await dialog.getByRole('button', { name: 'Undo' }).click();

		await expect.element(page.getByRole('dialog')).not.toBeInTheDocument();
		expect(api.calls(UNDO_ARTIST).map((call) => call.params)).toEqual([{ repair_id: 'repair-1' }]);
		expect(api.calls(UNDO)).toEqual([]);
	});

	it('says why a stale artist change cannot be undone', async () => {
		signIn('admin');
		fakeApi({
			[HISTORY]: { items: [repaired] },
			[UNDO_ARTIST]: apiError(
				409,
				'STALE_REVISION',
				"The album's artist changed after this, so it can't be undone."
			)
		});
		await renderWithQuery(AlbumHistory, { albumId: 'album-1' });

		await page.getByRole('button', { name: 'Undo' }).click();
		await page.getByRole('dialog').getByRole('button', { name: 'Undo' }).click();

		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent("The album's artist changed after this, so it can't be undone.");
	});

	it('offers no Undo to a listener', async () => {
		await show(repaired);
		await expect.element(page.getByText(/replacing "Unknown Artist"/)).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Undo' })).not.toBeInTheDocument();
	});
});
