import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { AlbumDetailView, TrackView } from '$lib/types';
import type { MembershipPreviewResponse } from '$lib/queries/library/LibraryOperationsTypes';

const album: AlbumDetailView = {
	id: 'album-1',
	title: 'Grouped Album',
	artist_name: 'Local Artist',
	artist_id: 'artist-1',
	release_group_mbid: 'rg-1',
	release_mbid: null,
	artist_mbid: null,
	identity_state: 'linked',
	track_count: 2,
	total_duration_seconds: 300,
	total_size_bytes: 1000,
	format: 'flac',
	year: 2024,
	is_compilation: false,
	cover_available: false,
	art_missing: false,
	favorite: false,
	date_added: 1,
	row_revision: 5,
	pending_review_id: null,
	contribution_id: null,
	contribution_state: null
};

function track(id: string, number: number): TrackView {
	return {
		id,
		title: `Track ${number}`,
		album_id: album.id,
		album_title: album.title,
		artist_id: album.artist_id,
		artist_name: album.artist_name,
		album_artist_id: album.artist_id,
		album_artist_name: album.artist_name,
		recording_mbid: null,
		release_group_mbid: album.release_group_mbid,
		artist_mbid: null,
		album_artist_mbid: null,
		disc_number: 1,
		track_number: number,
		year: 2024,
		genre: 'Rock',
		duration_seconds: 150,
		format: 'flac',
		bit_rate: null,
		sample_rate: null,
		bit_depth: null,
		channels: null,
		file_size_bytes: 500,
		date_added: 1,
		cover_available: false,
		art_missing: false,
		favorite: false,
		row_revision: 1
	};
}

const tracks = [track('track-1', 1), track('track-2', 2)];
const previewResult: MembershipPreviewResponse = {
	preview_token: 'preview-1',
	source_album_ids: ['album-1'],
	target_album_id: null,
	track_ids: ['track-1'],
	identity_conflicts: ['rg-1'],
	aliases: ['album-1'],
	automatic_groups: [],
	reference_counts: { playlists: 2, play_history: 7 },
	edition_changes: []
};

const h = vi.hoisted(() => ({
	previewData: undefined as MembershipPreviewResponse | undefined,
	preview: vi.fn(),
	apply: vi.fn(),
	previewKinds: [] as string[],
	applyKinds: [] as string[]
}));

vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibraryAlbumsQuery: () => ({ data: { items: [] } }),
	getLibraryAlbumDetailQuery: () => ({ data: undefined, refetch: vi.fn() })
}));
vi.mock('$lib/queries/library/LibraryCatalogMutations.svelte', () => ({
	createPreviewAlbumMembershipMutation: (kind: string) => {
		h.previewKinds.push(kind);
		return {
			mutateAsync: async (input: unknown) => {
				const result = await h.preview(input);
				h.previewData = previewResult;
				return result;
			},
			get data() {
				return h.previewData;
			},
			isPending: false,
			reset: () => {
				h.previewData = undefined;
			}
		};
	},
	createApplyAlbumMembershipMutation: (kind: string) => {
		h.applyKinds.push(kind);
		return { mutateAsync: h.apply, isPending: false };
	}
}));

import AlbumOrganizationDialog from './AlbumOrganizationDialog.svelte';

async function openSplitAndPreview(): Promise<void> {
	await page.getByText('Album organization').click();
	await page.getByRole('button', { name: 'Split album...' }).click();
	await page.getByRole('checkbox', { name: /1.1 Track 1/ }).click();
	await page.getByRole('button', { name: 'Preview changes' }).click();
}

beforeEach(() => {
	vi.clearAllMocks();
	h.previewData = undefined;
	h.previewKinds = [];
	h.applyKinds = [];
	h.preview.mockResolvedValue(previewResult);
	h.apply.mockResolvedValue({ kind: 'split' });
});

describe('AlbumOrganizationDialog', () => {
	it('keeps a stale grouping open and returns to Preview', async () => {
		h.apply.mockRejectedValue(new Error('stale revision'));
		await render(AlbumOrganizationDialog, {
			props: { album, tracks }
		} as unknown as Parameters<typeof render>[1]);
		await openSplitAndPreview();
		await page.getByRole('checkbox', { name: /preserves files and tags/ }).click();
		await page.getByRole('button', { name: 'Apply split album' }).click();
		await expect.element(page.getByText(/local grouping changed after this preview/)).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Preview changes' })).toBeVisible();
	});
});
