import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { MembershipPreviewResponse } from '$lib/queries/library/LibraryOperationsTypes';
import type {
	ArtistDuplicateGroupDetail,
	ArtistDuplicateGroupSummary,
	ArtistReconciliationProgress
} from '$lib/queries/artist-reconciliation/ArtistReconciliationTypes';

const memberOne = {
	id: 'artist-1',
	name: 'NIKI',
	sort_name: 'NIKI',
	row_revision: 3,
	provider_mbid: 'mbid-niki',
	album_credit_count: 2,
	track_credit_count: 10,
	primary_album_count: 2,
	favorite_count: 1,
	playlist_count: 1,
	history_count: 2,
	compatibility_id_count: 1,
	proven_credit_count: 12,
	active_credit_count: 12
};
const memberTwo = {
	...memberOne,
	id: 'artist-2',
	row_revision: 5,
	provider_mbid: null,
	album_credit_count: 1,
	track_credit_count: 4,
	primary_album_count: 1,
	proven_credit_count: 0,
	active_credit_count: 5
};

const group: ArtistDuplicateGroupSummary = {
	id: 'group-1',
	display_name: 'NIKI',
	state: 'waiting_for_identity',
	member_count: 2,
	members: [memberOne, memberTwo],
	provider_mbids: ['mbid-niki'],
	recommended_survivor_id: 'artist-1',
	affected_reference_count: 31,
	reason_code: 'INCOMPLETE_PROVIDER_PROOF',
	resolved_at: null
};

const detail: ArtistDuplicateGroupDetail = {
	...group,
	evidence: [
		{
			subject_kind: 'album',
			subject_id: 'album-1',
			subject_name: 'Moonchild',
			source_local_artist_id: 'artist-1',
			local_artist_id: 'artist-1',
			artist_mbid: 'mbid-niki',
			canonical_name: 'NIKI',
			credited_name: 'NIKI',
			join_phrase: '',
			release_mbid: 'release-1',
			release_track_mbid: null,
			album_identity_revision: 2,
			track_identity_revision: null,
			evidence_hash: 'evidence-1'
		}
	],
	releases: [
		{
			id: 'album-1',
			name: 'Moonchild',
			row_revision: 7,
			identity_ready: true,
			exact_track_mapping_ready: false
		}
	],
	tracks: [],
	reference_counts: {
		album_credits: 3,
		track_credits: 14,
		favorites: 2,
		playlist_snapshots: 2,
		history: 4,
		compatibility_ids: 2
	},
	member_revisions: { 'artist-1': 3, 'artist-2': 5 }
};

const progress: ArtistReconciliationProgress = {
	state: 'running',
	completed_count: 12,
	expected_count: 24,
	automatically_resolved_count: 14,
	waiting_for_identity_count: 49,
	genuine_review_count: 3,
	provider_conflict_count: 1,
	ambiguous_credit_structure_count: 1,
	same_name_only_count: 1,
	operation_job_id: 'job-1'
};

const previewResult: MembershipPreviewResponse = {
	preview_token: 'preview-1',
	source_album_ids: [],
	target_album_id: null,
	track_ids: [],
	identity_conflicts: [],
	aliases: ['artist-2'],
	automatic_groups: [],
	reference_counts: detail.reference_counts
};

const h = vi.hoisted(() => ({
	pageUrl: new URL('https://example.test/library/management/artists'),
	groups: [] as ArtistDuplicateGroupSummary[],
	detail: null as ArtistDuplicateGroupDetail | null,
	progress: null as ArtistReconciliationProgress | null,
	paramsGetter: (() => ({})) as () => { state?: string; search?: string },
	goto: vi.fn(),
	preview: vi.fn(),
	apply: vi.fn(),
	dismiss: vi.fn()
}));

vi.mock('$app/navigation', () => ({ goto: h.goto }));
vi.mock('$app/state', () => ({
	page: {
		get url() {
			return h.pageUrl;
		}
	}
}));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { isAdmin: true, user: { id: 'admin-1', role: 'admin' } }
}));
vi.mock('$lib/queries/artist-reconciliation/ArtistReconciliationQueries.svelte', () => ({
	getArtistReconciliationProgressQuery: () => ({
		get data() {
			return h.progress;
		},
		isLoading: false,
		isError: false
	}),
	getArtistDuplicateGroupsQuery: (getter: typeof h.paramsGetter) => {
		h.paramsGetter = getter;
		return {
			get data() {
				return {
					pages: [
						{
							items: h.groups,
							next_cursor: null,
							has_more: false,
							total: h.groups.length,
							counts: {}
						}
					]
				};
			},
			isLoading: false,
			isError: false,
			hasNextPage: false,
			isFetchingNextPage: false,
			fetchNextPage: vi.fn()
		};
	},
	getArtistDuplicateGroupQuery: () => ({
		get data() {
			return h.detail;
		},
		isLoading: false,
		isError: false
	})
}));
vi.mock('$lib/queries/artist-reconciliation/ArtistReconciliationMutations.svelte', () => ({
	dismissArtistDuplicateGroup: () => ({ mutateAsync: h.dismiss, isPending: false })
}));
vi.mock('$lib/queries/library/LibraryCatalogMutations.svelte', () => ({
	previewArtistMerge: () => ({ mutateAsync: h.preview, isPending: false }),
	applyArtistMerge: () => ({ mutateAsync: h.apply, isPending: false })
}));

import ArtistIdentityDesk from './ArtistIdentityDesk.svelte';

beforeEach(async () => {
	vi.clearAllMocks();
	await page.viewport(1280, 720);
	h.pageUrl = new URL('https://example.test/library/management/artists');
	h.groups = [group];
	h.detail = detail;
	h.progress = progress;
	h.preview.mockResolvedValue(previewResult);
	h.apply.mockResolvedValue({ surviving_artist_id: 'artist-1' });
	h.dismiss.mockResolvedValue({ group_id: 'group-1', dismissed_pairs: 1 });
});

describe('ArtistIdentityDesk', () => {
	it('shows progress and an evidence dossier with release work', async () => {
		await render(ArtistIdentityDesk);
		await expect.element(page.getByText('12 of 24 albums')).toBeVisible();
		await expect.element(page.getByText('14', { exact: true }).first()).toBeVisible();
		await page.getByRole('button', { name: /NIKI/ }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Exact provider evidence' }))
			.toBeVisible();
		await expect.element(page.getByText('Moonchild').first()).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Open Moonchild' }))
			.toHaveAttribute('href', '/album/album-1');
	});

	it('filters groups through the URL-backed query parameters', async () => {
		await render(ArtistIdentityDesk);
		expect(h.paramsGetter()).toEqual({ state: undefined, search: undefined });
		await page.getByLabelText('Search artist groups').fill('Grimes');
		await page.getByLabelText('Evidence state').selectOptions('provider_conflict');
		expect(h.paramsGetter()).toEqual({ state: undefined, search: undefined });
		await page.getByRole('button', { name: 'Apply filters' }).click();
		expect(h.goto).toHaveBeenCalledWith(
			'/library/management/artists?state=provider_conflict&q=Grimes',
			expect.objectContaining({ keepFocus: true, replaceState: true })
		);
	});

	it('restores applied filters from URL state', async () => {
		h.pageUrl = new URL(
			'https://example.test/library/management/artists?state=same_name_only&q=Grimes'
		);
		await render(ArtistIdentityDesk);
		expect(h.paramsGetter()).toEqual({ state: 'same_name_only', search: 'Grimes' });
		await expect.element(page.getByLabelText('Search artist groups')).toHaveValue('Grimes');
		await expect.element(page.getByLabelText('Evidence state')).toHaveValue('same_name_only');
	});

	it('places the selected inspector directly after its dossier on narrow screens', async () => {
		await page.viewport(390, 760);
		h.groups = [
			group,
			{
				...group,
				id: 'group-2',
				display_name: 'Grimes',
				members: [
					{ ...memberOne, id: 'artist-3', name: 'Grimes' },
					{ ...memberTwo, id: 'artist-4', name: 'Grimes' }
				]
			}
		];
		await render(ArtistIdentityDesk);

		const firstDossier = page.getByRole('button', { name: /NIKI/ }).first().element();
		const inspector = page
			.getByRole('complementary', { name: 'Artist identity inspector' })
			.element();
		const secondDossier = page.getByRole('button', { name: /Grimes/ }).element();
		expect(firstDossier.compareDocumentPosition(inspector) & Node.DOCUMENT_POSITION_FOLLOWING).toBe(
			Node.DOCUMENT_POSITION_FOLLOWING
		);
		expect(
			inspector.compareDocumentPosition(secondDossier) & Node.DOCUMENT_POSITION_FOLLOWING
		).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
	});

	it('previews and confirms the whole group while restoring focus', async () => {
		await render(ArtistIdentityDesk);
		await page.getByRole('button', { name: /NIKI/ }).click();
		const opener = page.getByRole('button', { name: 'Preview group merge' });
		await opener.click();
		await expect
			.element(page.getByRole('heading', { name: 'Confirm artist group merge' }))
			.toHaveFocus();
		const confirmation = page.getByRole('dialog');
		await expect.element(confirmation.getByText('Chosen survivor')).toBeVisible();
		await expect.element(confirmation.getByText('artist-1', { exact: true })).toBeVisible();
		await expect.element(confirmation.getByText('Retired records · 1')).toBeVisible();
		await expect.element(confirmation.getByText('artist-2', { exact: true })).toBeVisible();
		expect(h.preview).toHaveBeenCalledWith({
			source_artist_ids: ['artist-1', 'artist-2'],
			surviving_artist_id: 'artist-1',
			expected_revisions: { 'artist-1': 3, 'artist-2': 5 }
		});
		await page.getByRole('button', { name: 'Cancel' }).click();
		await expect.element(opener).toHaveFocus();
		await opener.click();
		await page.getByRole('checkbox', { name: /preserve retired IDs/ }).click();
		await page.getByRole('button', { name: 'Merge artists' }).click();
		expect(h.apply).toHaveBeenCalledWith(
			expect.objectContaining({ preview_token: 'preview-1', provider_choice: 'retain_survivor' })
		);
	});

	it('resets a prior detach choice before every new merge preview', async () => {
		h.preview.mockResolvedValue({ ...previewResult, identity_conflicts: ['mbid-conflict'] });
		await render(ArtistIdentityDesk);
		await page.getByRole('button', { name: /NIKI/ }).first().click();
		const opener = page.getByRole('button', { name: 'Preview group merge' });
		await opener.click();
		await page.getByRole('radio', { name: 'Detach conflicting provider identities' }).click();
		await page.getByRole('button', { name: 'Cancel' }).click();

		await opener.click();
		await expect
			.element(page.getByRole('radio', { name: "Keep the chosen survivor's provider identity" }))
			.toBeChecked();
		await page.getByRole('checkbox', { name: /preserve retired IDs/ }).click();
		await page.getByRole('button', { name: 'Merge artists' }).click();
		expect(h.apply).toHaveBeenCalledWith(
			expect.objectContaining({ provider_choice: 'retain_survivor' })
		);
	});

	it('keeps a stale preview safe and requires a new one', async () => {
		h.apply.mockRejectedValue(new Error('stale'));
		await render(ArtistIdentityDesk);
		await page.getByRole('button', { name: /NIKI/ }).click();
		await page.getByRole('button', { name: 'Preview group merge' }).click();
		await page.getByRole('checkbox', { name: /preserve retired IDs/ }).click();
		await page.getByRole('button', { name: 'Merge artists' }).click();
		await expect.element(page.getByText(/changed after preview/)).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Merge artists' })).toBeDisabled();
	});

	it('dismisses the exact member revisions after confirmation', async () => {
		await render(ArtistIdentityDesk);
		await page.getByRole('button', { name: /NIKI/ }).click();
		await page.getByRole('button', { name: 'Mark records as distinct' }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Keep these artists distinct?' }))
			.toHaveFocus();
		await expect.element(page.getByText(/If any of these records/)).toBeVisible();
		await page.getByRole('button', { name: 'Mark as distinct' }).click();
		expect(h.dismiss).toHaveBeenCalledWith({
			groupId: 'group-1',
			expectedMemberRevisions: { 'artist-1': 3, 'artist-2': 5 }
		});
	});

	it('has a deliberate empty state', async () => {
		h.groups = [];
		h.detail = null;
		await render(ArtistIdentityDesk);
		await expect
			.element(page.getByRole('heading', { name: 'No matching artist groups' }))
			.toBeVisible();
	});
});
