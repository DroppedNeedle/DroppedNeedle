import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { AlbumHistoryEntry } from '$lib/queries/library/LibraryAlbumHistoryQueries.svelte';

const h = vi.hoisted(() => ({
	query: {
		data: undefined as unknown,
		isPending: false,
		isError: false,
		error: null as unknown,
		refetch: () => undefined
	}
}));

vi.mock('$lib/queries/library/LibraryAlbumHistoryQueries.svelte', () => ({
	getAlbumHistoryQuery: () => h.query
}));

import AlbumHistory from './AlbumHistory.svelte';

const AT = Date.UTC(2026, 9, 12, 9, 30);

function entry(overrides: Partial<AlbumHistoryEntry>): AlbumHistoryEntry {
	return {
		kind: 'tracks_continued',
		at: AT,
		by: null,
		track_count: 0,
		track_titles: [],
		other_album_title: null,
		...overrides
	};
}

function show(...items: AlbumHistoryEntry[]) {
	h.query = { ...h.query, data: { items } };
	render(AlbumHistory, { albumId: 'album-1' });
}

const FIVE_OF_SIX = ['One', 'Two', 'Three', 'Four', 'Five'];

beforeEach(() => {
	h.query = { data: undefined, isPending: false, isError: false, error: null, refetch: () => {} };
});

describe('AlbumHistory', () => {
	it('tells each kind of entry in one line', async () => {
		show(
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
			entry({ kind: 'retag_joined', other_album_title: 'Old Name' }),
			entry({ kind: 'retag_joined', by: 'you', other_album_title: 'Old Name' }),
			entry({ kind: 'retag_joined', by: 'person', other_album_title: null }),
			entry({ kind: 'retag_half_left_alone', other_album_title: 'Old Name' }),
			entry({ kind: 'leftover_joined', other_album_title: 'Old Name' })
		);
		await expect.element(page.getByRole('heading', { name: 'Album history' })).toBeInTheDocument();
		const lines = [
			'"Airbag" has a new file, and its plays, favorites and playlist entries moved to it.',
			'6 tracks have new files, and their plays, favorites and playlist entries moved to them: "One", "Two", "Three", "Four", "Five" and 1 more.',
			'"Lucky" was replaced by the edition\'s version, and its plays, favorites and playlist entries moved to the new file.',
			'2 tracks were replaced by the edition\'s version, and their plays, favorites and playlist entries moved to the new files: "Lucky", "Karma".',
			'"Airbag" was replaced by other music, which is now a new track. Its plays, favorites and playlist entries stayed with the old track, which shows as missing.',
			'3 tracks were replaced by other music, which came in as new tracks. Their plays, favorites and playlist entries stayed with the old tracks, which show as missing: "Airbag", "Lucky", "Karma".',
			'Old Name was joined into this album after its files were retagged outside DroppedNeedle.',
			'You merged Old Name into this album.',
			'An admin merged Another album into this album.',
			'Left unmanaged like Old Name, whose files were being retagged as this album.',
			'Old Name, left over from an earlier version as a separate album, was folded into this album.'
		];
		for (const text of lines) {
			await expect.element(page.getByText(text, { exact: true })).toBeInTheDocument();
		}
		await expect.element(page.getByRole('button')).not.toBeInTheDocument();
	});

	it('leaves the titles out when none are sent', async () => {
		show(entry({ kind: 'tracks_continued', track_count: 3 }));
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
		show(entry({ kind: 'retag_joined', other_album_title: '<b>Bold</b>' }));
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
		show(entry({ kind: 'leftover_joined', other_album_title: 'Old Name' }));
		const time = page.getByTitle(new Date(AT).toLocaleString());
		await expect.element(time).toHaveAttribute('datetime', new Date(AT).toISOString());
	});

	it('shows nothing, not even the heading, when there is nothing to tell', async () => {
		show();
		await expect
			.element(page.getByRole('heading', { name: 'Album history' }))
			.not.toBeInTheDocument();
	});
});
