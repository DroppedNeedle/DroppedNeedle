import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { LibraryAlbumDetail, NativeTrackListItem } from '$lib/types';
import type { MembershipPreviewResponse } from '$lib/queries/library/LibraryOperationsTypes';

const album: LibraryAlbumDetail = {
	id: 'album-1',
	title: 'Grouped Album',
	artist_name: 'Local Artist',
	artist_id: 'artist-1',
	musicbrainz_release_group_id: 'rg-1',
	musicbrainz_release_id: null,
	musicbrainz_artist_id: null,
	album_identity_state: 'release_group_linked',
	track_count: 2,
	total_duration_seconds: 300,
	total_size_bytes: 1000,
	format: 'flac',
	year: 2024,
	is_compilation: false,
	cover_available: false,
	date_added: 1,
	sort_name: null,
	original_release_date: null,
	row_revision: 5,
	input_revision: 'input-5',
	identification_status: 'identified',
	review_id: null,
	review_revision: null,
	management_identity_readiness: 'ready',
	mapped_track_count: 2,
	management_identity_kind: null,
	custom_manifest_id: null,
	custom_manifest_version: null,
	custom_manifest_track_count: 0,
	custom_manifest_recognized_track_count: 0,
	custom_manifest_stale: false,
	management_excluded: false,
	management_exclusion_revision: null,
	management_excluded_at: null,
	active_edition_conversion: null,
	contribution_id: null,
	contribution_state: null,
	display_release_mbid: null,
	pick_basis: null
};

function track(id: string, number: number): NativeTrackListItem {
	return {
		id,
		title: `Track ${number}`,
		album_id: album.id,
		album_title: album.title,
		artist_id: album.artist_id,
		artist_name: album.artist_name,
		album_artist_id: album.artist_id,
		album_artist_name: album.artist_name,
		musicbrainz_recording_id: null,
		musicbrainz_release_group_id: album.musicbrainz_release_group_id,
		musicbrainz_artist_id: null,
		musicbrainz_album_artist_id: null,
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
		current_tier: null,
		below_cutoff: false
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
	reference_counts: { playlists: 2, play_history: 7 }
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
	getLibraryAlbumDetailQuery: () => ({ data: undefined })
}));
vi.mock('$lib/queries/library/LibraryCatalogMutations.svelte', () => ({
	previewAlbumMembership: (kind: string) => {
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
	applyAlbumMembership: (kind: string) => {
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
	it('previews exact membership and states that files and tags stay unchanged', async () => {
		await render(AlbumOrganizationDialog, {
			props: { album, tracks }
		} as unknown as Parameters<typeof render>[1]);
		await openSplitAndPreview();
		await expect.element(page.getByText(/will not move files or rewrite tags/)).toBeVisible();
		expect(h.preview).toHaveBeenCalledWith({
			albumId: 'album-1',
			request: {
				track_ids: ['track-1'],
				expected_album_revisions: { 'album-1': 5 },
				target_album_id: null
			}
		});
		await expect.element(page.getByText(/1 tracks/)).toBeVisible();
		await expect.element(page.getByText(/External identities conflict/)).toBeVisible();
		await page.getByRole('radio', { name: /Retain the target identity/ }).click();
		await page.getByRole('checkbox', { name: /preserves files and tags/ }).click();
		await page.getByRole('button', { name: 'Apply split album' }).click();
		expect(h.apply).toHaveBeenCalledWith({
			albumId: 'album-1',
			request: {
				track_ids: ['track-1'],
				expected_album_revisions: { 'album-1': 5 },
				target_album_id: null
			},
			previewToken: 'preview-1',
			identityChoice: 'retain_manual'
		});
	});

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

	it('returns focus to the exact organization action after cancellation', async () => {
		await render(AlbumOrganizationDialog, {
			props: { album, tracks }
		} as unknown as Parameters<typeof render>[1]);
		await page.getByText('Album organization').click();
		const opener = page.getByRole('button', { name: 'Reset manual grouping...' });
		await opener.click();
		await expect
			.element(page.getByRole('heading', { name: 'Reset manual grouping' }))
			.toHaveFocus();
		await page.getByRole('button', { name: 'Cancel' }).click();
		await expect.element(opener).toHaveFocus();
	});
});
