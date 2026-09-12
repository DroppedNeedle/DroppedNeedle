import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { LibraryContribution } from '$lib/types';

const h = vi.hoisted(() => ({
	mutateSave: vi.fn(),
	mutateRebuild: vi.fn(),
	mutateCancel: vi.fn(),
	mutateSelect: vi.fn(),
	authStore: { isTrusted: true },
	goto: vi.fn()
}));

vi.mock('$app/navigation', () => ({ goto: (...args: unknown[]) => h.goto(...args) }));
vi.mock('$lib/stores/authStore.svelte', () => ({ authStore: h.authStore }));

const contribution: LibraryContribution = {
	id: 'contribution-1',
	local_album_id: 'album-1',
	created_by_user_id: 'curator-1',
	updated_by_user_id: 'curator-1',
	state: 'draft',
	album_row_revision: 1,
	input_revision: 'input-1',
	local_snapshot: {
		schema_version: 1,
		local_album_id: 'album-1',
		local_artist_id: 'artist-1',
		album_row_revision: 1,
		input_revision: 'input-1',
		title: 'Basement Pressing',
		album_artist_name: 'Signal Path',
		artist_kind: 'group',
		musicbrainz_artist_id: null,
		musicbrainz_release_group_id: null,
		musicbrainz_release_id: null,
		release_date: '2024',
		year: 2024,
		is_compilation: false,
		captured_at: 1_700_000_000,
		media: [
			{
				position: 1,
				title: null,
				tracks: [
					{
						local_track_id: 'track-1',
						disc_number: 1,
						track_number: 1,
						title: 'First Track',
						artist_name: 'Signal Path',
						duration_seconds: 180,
						duration_reliable: true
					}
				]
			}
		]
	},
	draft: {
		schema_version: 1,
		title: { value: 'Basement Pressing', source: 'local' },
		artist_credit: { value: 'Signal Path', source: 'local' },
		release_date: { value: '2024', source: 'local' },
		country: { value: null, source: 'local' },
		label: { value: null, source: 'local' },
		catalogue_number: { value: null, source: 'local' },
		barcode: { value: null, source: 'local' },
		packaging: { value: null, source: 'local' },
		media: [
			{
				position: 1,
				title: { value: null, source: 'local' },
				format: { value: null, source: 'local' },
				tracks: [
					{
						local_track_id: 'track-1',
						disc_number: 1,
						track_number: 1,
						title: { value: 'First Track', source: 'local' },
						artist_name: { value: 'Signal Path', source: 'local' },
						duration_seconds: 180
					}
				]
			}
		]
	},
	source_selection: { schema_version: 1, sources: [], alignments: [] },
	provider_snapshot_expires_at: null,
	discogs_source: null,
	duplicate_result: null,
	duplicate_checked_at: null,
	result_release_mbid: null,
	result_source: null,
	result_received_at: null,
	seeded_at: null,
	terminal_at: null,
	created_at: 1,
	updated_at: 1,
	row_revision: 1,
	input_is_current: true,
	validation: [],
	next_actions: ['edit_draft', 'run_duplicate_check', 'cancel']
};

vi.mock('$lib/queries/libraryContributions/LibraryContributionQueries.svelte', () => ({
	getLibraryContributionQuery: () => ({
		data: contribution,
		isLoading: false,
		isError: false,
		refetch: vi.fn()
	})
}));

vi.mock('$lib/queries/libraryContributions/LibraryContributionMutations.svelte', () => ({
	updateLibraryContributionMutation: () => ({ isPending: false, mutate: h.mutateSave }),
	rebuildLibraryContributionMutation: () => ({ isPending: false, mutate: h.mutateRebuild }),
	cancelLibraryContributionMutation: () => ({ isPending: false, mutate: h.mutateCancel }),
	searchDiscogsReleasesMutation: () => ({ isPending: false, mutate: vi.fn(), data: undefined }),
	selectDiscogsReleaseMutation: () => ({ isPending: false, mutate: h.mutateSelect }),
	removeDiscogsReleaseMutation: () => ({ isPending: false, mutate: vi.fn() }),
	checkMusicBrainzDuplicatesMutation: () => ({ isPending: false, mutate: vi.fn() }),
	attachExistingMusicBrainzReleaseMutation: () => ({ isPending: false, mutate: vi.fn() }),
	createMusicBrainzSeedMutation: () => ({ isPending: false, mutateAsync: vi.fn() }),
	recordMusicBrainzResultMutation: () => ({ isPending: false, mutate: vi.fn() }),
	retryMusicBrainzVerificationMutation: () => ({ isPending: false, mutate: vi.fn() })
}));

import ContributionPage from './+page.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.authStore.isTrusted = true;
});

describe('library contribution page', () => {
	it('presents the local proof sheet without filesystem data', async () => {
		await render(ContributionPage, {
			props: { data: { contributionId: contribution.id, primarySource: 'listenbrainz' } }
		} as unknown as Parameters<typeof render>[1]);

		await expect.element(page.getByRole('heading', { name: 'Basement Pressing' })).toBeVisible();
		await expect.element(page.getByRole('heading', { name: 'Local metadata' })).toBeVisible();
		await expect.element(page.getByText('1 medium · 1 track')).toBeVisible();
		await expect.element(page.getByText('/private/music')).not.toBeInTheDocument();
	});

	it('marks edits as entered here and saves against the current revision', async () => {
		await render(ContributionPage, {
			props: { data: { contributionId: contribution.id, primarySource: 'listenbrainz' } }
		} as unknown as Parameters<typeof render>[1]);

		const title = page.getByRole('textbox', { name: 'Release title' });
		await title.fill('Corrected pressing');
		await page.getByRole('button', { name: 'Save draft' }).click();

		expect(h.mutateSave).toHaveBeenCalledWith(
			expect.objectContaining({
				contributionId: 'contribution-1',
				expectedRowRevision: 1,
				draft: expect.objectContaining({
					title: { value: 'Corrected pressing', source: 'entered_here' }
				})
			})
		);
	});

	it('blocks provider actions until visible draft edits are saved or discarded', async () => {
		await render(ContributionPage, {
			props: { data: { contributionId: contribution.id, primarySource: 'listenbrainz' } }
		} as unknown as Parameters<typeof render>[1]);

		await page.getByRole('textbox', { name: 'Release title' }).fill('Unsaved title');

		await expect
			.element(page.getByText('Save this draft before checking provider data'))
			.toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Search' })).toBeDisabled();
		await expect.element(page.getByRole('button', { name: 'Check MusicBrainz' })).toBeDisabled();
	});

	it('uses an exact Discogs ID through the deterministic selection action', async () => {
		await render(ContributionPage, {
			props: { data: { contributionId: contribution.id, primarySource: 'listenbrainz' } }
		} as unknown as Parameters<typeof render>[1]);

		await page.getByRole('textbox', { name: 'Discogs release' }).fill('249504');
		await page.getByRole('button', { name: 'Use release' }).click();

		expect(h.mutateSelect).toHaveBeenCalledWith({
			contributionId: 'contribution-1',
			expectedRowRevision: 1,
			releaseIdOrUrl: '249504'
		});
	});

	it('shows shared contribution status without curator controls to listeners', async () => {
		h.authStore.isTrusted = false;
		await render(ContributionPage, {
			props: { data: { contributionId: contribution.id, primarySource: 'listenbrainz' } }
		} as unknown as Parameters<typeof render>[1]);

		await expect.element(page.getByRole('textbox', { name: 'Release title' })).toBeDisabled();
		await expect.element(page.getByRole('button', { name: 'Save draft' })).not.toBeInTheDocument();
		await expect.element(page.getByRole('button', { name: 'Cancel' })).not.toBeInTheDocument();
	});
});
