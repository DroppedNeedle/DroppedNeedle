import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { LibraryContribution, MusicBrainzSeed } from '$lib/types';

const h = vi.hoisted(() => ({
	check: vi.fn(),
	attach: vi.fn(),
	seed: vi.fn(),
	recordResult: vi.fn(),
	retry: vi.fn(),
	closePopup: vi.fn()
}));

vi.mock('$lib/queries/libraryContributions/LibraryContributionMutations.svelte', () => ({
	checkMusicBrainzDuplicatesMutation: () => ({ isPending: false, mutate: h.check }),
	attachExistingMusicBrainzReleaseMutation: () => ({ isPending: false, mutate: h.attach }),
	createMusicBrainzSeedMutation: () => ({ isPending: false, mutateAsync: h.seed }),
	recordMusicBrainzResultMutation: () => ({ isPending: false, mutate: h.recordResult }),
	retryMusicBrainzVerificationMutation: () => ({ isPending: false, mutate: h.retry })
}));

import ContributionMusicBrainzReview from './ContributionMusicBrainzReview.svelte';

const contribution = (overrides: Partial<LibraryContribution> = {}): LibraryContribution => ({
	id: 'contribution-1',
	local_album_id: 'album-1',
	created_by_user_id: 'curator-1',
	updated_by_user_id: 'curator-1',
	state: 'ready',
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
		media: []
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
		media: []
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
	row_revision: 4,
	input_is_current: true,
	validation: [],
	next_actions: ['edit_draft', 'run_duplicate_check', 'cancel'],
	...overrides
});

beforeEach(() => {
	vi.clearAllMocks();
});

describe('ContributionMusicBrainzReview', () => {
	it('runs the duplicate check against the current revision', async () => {
		await render(ContributionMusicBrainzReview, { contribution: contribution() });

		await page.getByRole('button', { name: 'Check MusicBrainz' }).click();

		expect(h.check).toHaveBeenCalledWith({
			contributionId: 'contribution-1',
			expectedRowRevision: 4,
			differentEditionConfirmed: false
		});
	});

	it('blocks creation and offers attachment for one exact Discogs relationship', async () => {
		const exact = contribution({
			state: 'needs_review',
			duplicate_result: {
				schema_version: 1,
				checked_at: 2,
				input_revision: 'input-1',
				different_edition_confirmed: false,
				candidates: [
					{
						release_mbid: 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa',
						release_group_mbid: 'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb',
						title: 'Basement Pressing',
						artist_name: 'Signal Path',
						evidence_kind: 'exact_discogs_url',
						exact: true,
						differences: []
					}
				]
			},
			next_actions: ['edit_draft', 'run_duplicate_check', 'attach_existing', 'cancel']
		});
		await render(ContributionMusicBrainzReview, { contribution: exact });

		await expect.element(page.getByText('Exact match found')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Continue on MusicBrainz' }))
			.not.toBeInTheDocument();
		await page.getByRole('button', { name: 'Use this MusicBrainz release' }).click();

		expect(h.attach).toHaveBeenCalledWith({
			contributionId: 'contribution-1',
			expectedRowRevision: 4,
			releaseMbid: 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa'
		});
	});

	it('posts every ordered seed field to the fixed MusicBrainz editor', async () => {
		const fields = [
			{ name: 'name', value: 'Basement & Pressing' },
			{ name: 'mediums.0.track.0.name', value: 'One' },
			{ name: 'mediums.0.track.0.name', value: 'One (duplicate field)' },
			{ name: 'redirect_uri', value: 'https://example.test/callback?token=a%2Bb' }
		];
		const seed: MusicBrainzSeed = {
			action_url: 'https://musicbrainz.org/release/add',
			method: 'POST',
			fields,
			contribution_revision: 5,
			expires_at: 1_900_000_000
		};
		h.seed.mockResolvedValue(seed);
		const popup = {
			opener: window,
			document: { title: '', body: { textContent: '' } },
			close: h.closePopup
		} as unknown as Window;
		vi.spyOn(window, 'open').mockReturnValue(popup);
		let submitted:
			| {
					action: string;
					method: string;
					target: string;
					entries: Array<[string, FormDataEntryValue]>;
			  }
			| undefined;
		vi.spyOn(HTMLFormElement.prototype, 'submit').mockImplementation(function (
			this: HTMLFormElement
		) {
			submitted = {
				action: this.action,
				method: this.method,
				target: this.target,
				entries: Array.from(new FormData(this).entries())
			};
		});
		await render(ContributionMusicBrainzReview, {
			contribution: contribution({
				state: 'seeded',
				duplicate_result: {
					schema_version: 1,
					checked_at: 2,
					input_revision: 'input-1',
					candidates: [],
					different_edition_confirmed: false
				},
				next_actions: ['seed_musicbrainz', 'cancel']
			})
		});

		await page.getByRole('button', { name: 'Continue again' }).click();

		expect(h.seed).toHaveBeenCalledWith({
			contributionId: 'contribution-1',
			expectedRowRevision: 4
		});
		expect(submitted).toEqual({
			action: 'https://musicbrainz.org/release/add',
			method: 'post',
			target: 'droppedneedle-musicbrainz-contribution-1',
			entries: fields.map((field) => [field.name, field.value])
		});
	});

	it('shows a returned MBID as pending while verification runs', async () => {
		await render(ContributionMusicBrainzReview, {
			contribution: contribution({
				state: 'verifying',
				result_release_mbid: 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa',
				next_actions: ['cancel']
			})
		});

		await expect.element(page.getByText('MusicBrainz release returned')).toBeVisible();
		await expect.element(page.getByText('This page updates automatically.')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Continue on MusicBrainz' }))
			.not.toBeInTheDocument();
	});

	it('validates and queues a pasted MusicBrainz recovery result locally', async () => {
		await render(ContributionMusicBrainzReview, {
			contribution: contribution({
				state: 'seeded',
				duplicate_result: {
					schema_version: 1,
					checked_at: 2,
					input_revision: 'input-1',
					candidates: [],
					different_edition_confirmed: false
				},
				next_actions: ['seed_musicbrainz', 'cancel']
			})
		});
		const input = page.getByRole('textbox', { name: 'MusicBrainz release MBID or URL' });

		await input.fill('https://evil.example/release/aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa');
		await page.getByRole('heading', { name: 'Already submitted the release?' }).click();
		await expect
			.element(page.getByText('Enter a release MBID or an official musicbrainz.org release URL.'))
			.toBeVisible();
		await input.fill('https://musicbrainz.org/release/AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA');
		await page.getByRole('button', { name: 'Verify result' }).click();

		expect(h.recordResult).toHaveBeenCalledWith({
			contributionId: 'contribution-1',
			expectedRowRevision: 4,
			releaseIdOrUrl: 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa',
			replaceExistingResult: false
		});
	});

	it('explicitly replaces a rejected result before re-verification', async () => {
		await render(ContributionMusicBrainzReview, {
			contribution: contribution({
				state: 'needs_review',
				result_release_mbid: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
				next_actions: ['retry_verification', 'cancel']
			})
		});
		const replacement = 'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb';

		await page.getByRole('textbox', { name: 'MusicBrainz release MBID or URL' }).fill(replacement);
		await page.getByRole('button', { name: 'Replace and verify' }).click();

		expect(h.recordResult).toHaveBeenCalledWith({
			contributionId: 'contribution-1',
			expectedRowRevision: 4,
			releaseIdOrUrl: replacement,
			replaceExistingResult: true
		});
	});
});
