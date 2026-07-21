import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	query: { data: undefined, isLoading: false, isError: false } as Record<string, unknown>,
	keep: vi.fn(),
	detach: vi.fn(),
	exclude: vi.fn(),
	restore: vi.fn(),
	retry: vi.fn(),
	accept: vi.fn()
}));

vi.mock('$lib/queries/library/LibraryReviewQueries.svelte', () => ({
	getLibraryReviewQuery: () => h.query
}));
vi.mock('$lib/queries/library/LibraryReviewMutations.svelte', () => ({
	actOnLibraryReview: (action: string) => ({
		mutateAsync:
			action === 'keep_tagged'
				? h.keep
				: action === 'detach_keep_tagged'
					? h.detach
					: action === 'exclude'
						? h.exclude
						: h.restore,
		isPending: false
	}),
	acceptLibraryReviewCandidate: () => ({ mutateAsync: h.accept, isPending: false }),
	retryLibraryReview: () => ({ mutateAsync: h.retry, isPending: false })
}));
import LibraryReviewDetail from './LibraryReviewDetail.svelte';

function detail(identity = false) {
	return {
		review: {
			id: 'review-1',
			state: 'needs_review',
			reason_code: 'CONTRADICTORY',
			local_album_id: 'local-album-1',
			local_track_id: null,
			album_title: 'The Local Album',
			album_artist_name: 'Various Artists',
			year: 2024,
			track_count: 3,
			metadata_incomplete_count: 0,
			root_id: 'root-1',
			relative_path: 'album/track.flac',
			effective_policy: 'automatic',
			exclusion_source: null,
			release_group_mbid: identity ? 'provider-rg-1' : null,
			identity_source: identity ? 'automatic' : null,
			candidate_count: 2,
			evidence_summary: {},
			active_job_state: null,
			created_at: 1,
			updated_at: 2,
			row_revision: 4
		},
		tracks: [
			{
				id: 'track-1',
				title: 'Song',
				artist_name: 'Track Artist',
				local_artist_id: 'local-artist-1',
				relative_path: 'album/track.flac',
				disc_number: 1,
				track_number: 1,
				availability: 'indexed',
				membership_locked: false,
				recording_mbid: identity ? 'current-recording-1' : null
			},
			{
				id: 'track-2',
				title: 'Unknown Song',
				artist_name: 'Track Artist',
				local_artist_id: 'local-artist-1',
				relative_path: 'album/track-2.flac',
				disc_number: 1,
				track_number: 2,
				availability: 'indexed',
				membership_locked: false,
				recording_mbid: null
			},
			{
				id: 'track-3',
				title: 'Matching Song',
				artist_name: 'Track Artist',
				local_artist_id: 'local-artist-1',
				relative_path: 'album/track-3.flac',
				disc_number: 1,
				track_number: 3,
				availability: 'indexed',
				membership_locked: false,
				recording_mbid: identity ? 'current-recording-3' : null
			}
		],
		current_evidence: null,
		candidates: [
			{
				candidate_key: 'candidate-safe',
				evidence_revision: 'evidence-1',
				automatic_safe: true,
				evidence: {
					release_group_mbid: 'rg-safe',
					release_mbid: null,
					album_title: 'Safe Release',
					album_artist_name: 'Artist',
					artist_mbid: null,
					release_type: 'album',
					release_date: '2024',
					local_album_title: 'The Local Album',
					local_album_artist_name: 'Various Artists',
					album_title_classification: 'supported',
					album_artist_classification: 'supported',
					track_evidence: [],
					unmatched_expected_tracks: [],
					score: 0.95,
					margin: 0.3,
					reason_code: 'SUPPORTED',
					matcher_version: 'v1'
				}
			},
			{
				candidate_key: 'candidate-manual',
				evidence_revision: 'evidence-2',
				automatic_safe: false,
				evidence: {
					release_group_mbid: 'rg-manual',
					release_mbid: 'release-manual',
					album_title: 'Manual Release',
					album_artist_name: 'Artist',
					artist_mbid: null,
					release_type: 'album',
					release_date: '2024',
					local_album_title: 'The Local Album',
					local_album_artist_name: 'Various Artists',
					album_title_classification: 'contradictory',
					album_artist_classification: 'unknown',
					track_evidence: [
						{
							local_track_id: 'track-1',
							classification: 'contradictory',
							evidence_kinds: ['recording_id'],
							candidate_track_title: 'Different Song',
							candidate_disc_number: 1,
							candidate_track_position: 1,
							recording_mbid: 'recording-manual'
						},
						{
							local_track_id: 'track-2',
							classification: 'unknown',
							evidence_kinds: [],
							candidate_track_title: null,
							candidate_disc_number: null,
							candidate_track_position: null,
							recording_mbid: null
						},
						{
							local_track_id: 'track-3',
							classification: 'supported',
							evidence_kinds: ['recording_id'],
							candidate_track_title: 'Matching Song',
							candidate_disc_number: 1,
							candidate_track_position: 3,
							recording_mbid: 'recording-supported'
						}
					],
					unmatched_expected_tracks: ['Missing Song'],
					score: 0.6,
					margin: 0.01,
					reason_code: 'CONTRADICTORY',
					matcher_version: 'v1'
				}
			}
		],
		supported: [
			{
				local_track_id: 'track-1',
				classification: 'supported',
				evidence_kinds: [],
				candidate_track_title: 'Song',
				candidate_disc_number: 1,
				candidate_track_position: 1,
				recording_mbid: null
			}
		],
		unknown: [],
		contradictory: [],
		history: [],
		available_actions: identity
			? ['detach_keep_tagged', 'retry', 'exclude', 'accept_candidate']
			: ['keep_tagged', 'retry', 'exclude', 'accept_candidate'],
		catalog_revision: 9,
		album_revision: 3,
		input_revision: 'input-1',
		evidence_revision: 'evidence-1',
		identity_revision: identity ? 7 : null,
		job_revision: null
	};
}

beforeEach(() => {
	vi.clearAllMocks();
	h.query = { data: detail(false), isLoading: false, isError: false };
});

describe('LibraryReviewDetail', () => {
	function disableRandomUuid(): () => void {
		const descriptor = Object.getOwnPropertyDescriptor(crypto, 'randomUUID');
		Object.defineProperty(crypto, 'randomUUID', { configurable: true, value: undefined });
		return () => {
			if (descriptor) Object.defineProperty(crypto, 'randomUUID', descriptor);
			else Reflect.deleteProperty(crypto, 'randomUUID');
		};
	}

	it('shows stable local links, compilation credit, evidence, and safe/manual candidate actions', async () => {
		render(LibraryReviewDetail, {
			props: { reviewId: 'review-1', onclose: vi.fn() }
		} as unknown as Parameters<typeof render>[1]);
		await expect.element(page.getByRole('heading', { name: 'The Local Album' })).toBeVisible();
		await expect.element(page.getByText(/Various Artists/).first()).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Track Artist' }).first())
			.toHaveAttribute('href', '/artist/local-artist-1');
		await expect.element(page.getByText('Supports this release')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Use this release' })).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Use anyway...' })).toBeVisible();
	});

	it('offers plain Keep only without an external identity', async () => {
		render(LibraryReviewDetail, {
			props: { reviewId: 'review-1', onclose: vi.fn() }
		} as unknown as Parameters<typeof render>[1]);
		await expect.element(page.getByRole('button', { name: 'Keep as tagged' })).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Detach and keep as tagged...' }))
			.not.toBeInTheDocument();
	});

	it('requires the explicit detach preview when an identity exists', async () => {
		h.query = { data: detail(true), isLoading: false, isError: false };
		render(LibraryReviewDetail, {
			props: { reviewId: 'review-1', onclose: vi.fn() }
		} as unknown as Parameters<typeof render>[1]);
		await expect
			.element(page.getByRole('button', { name: 'Keep as tagged', exact: true }))
			.not.toBeInTheDocument();
		const opener = page.getByRole('button', { name: 'Detach and keep as tagged...' });
		await opener.click();
		const confirmation = page.getByRole('dialog', {
			name: 'Detach identity and keep local metadata?'
		});
		await expect
			.element(
				confirmation.getByRole('heading', { name: 'Detach identity and keep local metadata?' })
			)
			.toHaveFocus();
		await expect.element(confirmation.getByText(/The Local Album.*local-album-1/)).toBeVisible();
		await expect.element(confirmation.getByText(/provider-rg-1/)).toBeVisible();
		await expect
			.element(confirmation.getByText(/Song.*track-1.*current-recording-1/))
			.toBeVisible();
		await expect
			.element(confirmation.getByText(/Matching Song.*track-3.*current-recording-3/))
			.toBeVisible();
		await expect
			.element(
				confirmation.getByText(/local IDs, playback, playlists, history, favorites, and artwork/)
			)
			.toBeVisible();
		await page.getByRole('button', { name: 'Cancel' }).click();
		await expect.element(opener).toHaveFocus();
		await opener.click();
		await page.getByRole('button', { name: 'Detach identity and keep local metadata' }).click();
		expect(h.detach).toHaveBeenCalledWith(
			expect.objectContaining({
				reviewId: 'review-1',
				body: expect.objectContaining({
					confirmation: true,
					expected_catalog_revision: 9,
					expected_identity_revision: 7
				})
			})
		);
	});

	it('requires confirmation before accepting a conflicting candidate', async () => {
		render(LibraryReviewDetail, {
			props: { reviewId: 'review-1', onclose: vi.fn() }
		} as unknown as Parameters<typeof render>[1]);

		await page.getByRole('button', { name: 'Use anyway...' }).click();
		expect(h.accept).not.toHaveBeenCalled();
		await expect
			.element(page.getByRole('heading', { name: 'Use this release despite conflicts?' }))
			.toHaveFocus();
		await expect
			.element(page.getByText('The local evidence conflicts with this release'))
			.toBeVisible();
		await expect
			.element(page.getByRole('heading', { name: 'Failed evidence gates' }))
			.toBeVisible();
		await expect.element(page.getByText('Song (track-1)')).toBeVisible();
		await expect.element(page.getByText('Unknown Song (track-2)')).toBeVisible();
		await expect.element(page.getByText('Release group: rg-manual')).toBeVisible();
		await expect.element(page.getByText('Release: release-manual')).toBeVisible();
		await expect
			.element(page.getByText(/Matching Song.*track-3.*recording-supported/))
			.toBeVisible();
		await expect.element(page.getByText(/durable manual identity/)).toBeVisible();
		await page.getByRole('button', { name: 'Use conflicting release' }).click();
		expect(h.accept).toHaveBeenCalledWith(
			expect.objectContaining({
				body: expect.objectContaining({
					candidate_key: 'candidate-manual',
					confirmation: true
				})
			})
		);
	});

	it('starts retry with a fallback v4 UUID and catches a network rejection', async () => {
		const restoreRandomUuid = disableRandomUuid();
		const unhandled: PromiseRejectionEvent[] = [];
		const recordUnhandled = (event: PromiseRejectionEvent) => unhandled.push(event);
		window.addEventListener('unhandledrejection', recordUnhandled);
		try {
			h.retry.mockRejectedValueOnce(new TypeError('Failed to fetch'));
			render(LibraryReviewDetail, {
				props: { reviewId: 'review-1', onclose: vi.fn() }
			} as unknown as Parameters<typeof render>[1]);
			await page.getByRole('button', { name: 'Retry identification' }).first().click();
			const confirmation = page.getByRole('dialog', { name: 'Retry identification?' });
			await confirmation.getByRole('button', { name: 'Retry identification' }).click();
			await expect.element(confirmation).toBeVisible();
			const request = h.retry.mock.calls[0]?.[0];
			expect(request).toEqual(
				expect.objectContaining({
					reviewId: 'review-1',
					body: expect.objectContaining({ confirmation: true })
				})
			);
			expect(request.body.idempotency_key).toMatch(
				/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
			);
			await new Promise((resolve) => setTimeout(resolve, 0));
			expect(unhandled).toEqual([]);
		} finally {
			window.removeEventListener('unhandledrejection', recordUnhandled);
			restoreRandomUuid();
		}
	});
});
