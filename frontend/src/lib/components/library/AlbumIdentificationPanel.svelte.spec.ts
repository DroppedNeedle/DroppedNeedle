import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { ApiError } from '$lib/api/client';
import type { LibraryAlbumDetail } from '$lib/types';
import type { OperationResponse } from '$lib/queries/library/LibraryOperationsTypes';
import type { EditionConversionStatus } from '$lib/queries/library/EditionConversionQueries.svelte';

const album: LibraryAlbumDetail = {
	id: 'album-1',
	title: 'Local Signals',
	artist_name: 'Signal Artist',
	artist_id: 'artist-1',
	musicbrainz_release_group_id: null,
	musicbrainz_release_id: null,
	musicbrainz_artist_id: null,
	album_identity_state: 'local_only',
	track_count: 2,
	total_duration_seconds: 300,
	total_size_bytes: 1000,
	format: 'flac',
	year: 2024,
	is_compilation: false,
	cover_available: true,
	date_added: 1,
	sort_name: null,
	original_release_date: null,
	row_revision: 5,
	input_revision: 'input-5',
	identification_status: 'local_metadata',
	review_id: null,
	review_revision: null,
	management_identity_readiness: 'exact_release_required',
	mapped_track_count: 0,
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

function job(overrides: Partial<OperationResponse> = {}): OperationResponse {
	return {
		id: 'job-1',
		kind: 'explicit_reidentification',
		state: 'running',
		expected_work_count: 2,
		completed_count: 1,
		succeeded_count: 0,
		failed_count: 0,
		skipped_count: 0,
		control_request: 'none',
		terminal_code: null,
		row_revision: 8,
		event_revision: 2,
		created_at: 1,
		updated_at: 2,
		results: [],
		results_truncated: false,
		repair_summary: null,
		reidentification_candidates: [],
		selected_reidentification_candidate_key: null,
		...overrides
	};
}

const candidateJob = job({
	state: 'ready',
	completed_count: 2,
	reidentification_candidates: [
		{
			candidate_key: 'rg-1:release-1',
			evidence_revision: 'evidence-1',
			automatic_safe: true,
			evidence: {
				release_group_mbid: 'rg-1',
				release_mbid: 'release-1',
				album_title: 'The Right Release',
				album_artist_name: 'Signal Artist',
				artist_mbid: null,
				release_type: 'album',
				release_date: '2024',
				local_album_title: 'Local Signals',
				local_album_artist_name: 'Signal Artist',
				album_title_classification: 'supported',
				album_artist_classification: 'supported',
				score: 0.98,
				margin: 0.4,
				reason_code: 'COMPLETE_SUPPORT',
				matcher_version: 'v1',
				track_evidence: [
					{
						local_track_id: 'track-1',
						classification: 'supported',
						evidence_kinds: ['release_track_id'],
						candidate_track_title: 'First Song',
						candidate_disc_number: 1,
						candidate_track_position: 1,
						recording_mbid: 'recording-1',
						release_track_mbid: 'release-track-1'
					},
					{
						local_track_id: 'track-2',
						classification: 'supported',
						evidence_kinds: ['release_track_id'],
						candidate_track_title: 'Second Song',
						candidate_disc_number: 1,
						candidate_track_position: 2,
						recording_mbid: 'recording-2',
						release_track_mbid: 'release-track-2'
					}
				],
				unmatched_expected_tracks: []
			}
		}
	]
} as unknown as Partial<OperationResponse>);

const conversionPreflightStatus: EditionConversionStatus = {
	job_id: 'conversion-1',
	local_album_id: 'album-1',
	release_group_mbid: 'rg-1',
	release_mbid: 'release-1',
	album_title: 'The Right Release',
	artist_name: 'Signal Artist',
	state: 'preflight',
	download_source_ready: true,
	required_temporary_bytes: 1000,
	kept_count: 1,
	acquire_count: 1,
	recycle_count: 1,
	staged_count: 0,
	failed_count: 0,
	row_revision: 3,
	created_at: 1,
	updated_at: 1,
	targets: [],
	local_files: [],
	final_preview_job_id: null,
	preflight_token: null,
	error_code: null
};

const h = vi.hoisted(() => ({
	jobs: {} as Record<string, OperationResponse>,
	getJobId: (() => null) as () => string | null,
	start: vi.fn(),
	select: vi.fn(),
	pause: vi.fn(),
	resume: vi.fn(),
	stop: vi.fn(),
	resetSelect: vi.fn(),
	selectError: null as ApiError | null,
	queryError: false,
	conversionData: null as EditionConversionStatus | null,
	conversionPreflightData: null as EditionConversionStatus | null,
	conversionStart: vi.fn(),
	conversionRefetch: vi.fn()
}));

vi.mock('$lib/stores/authStore.svelte', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/stores/authStore.svelte')>()),
	authStore: { user: { id: 'admin-1' } }
}));
vi.mock('$lib/queries/library/LibraryOperationQueries.svelte', () => ({
	getLibraryOperationQuery: (getId: () => string | null) => {
		h.getJobId = getId;
		return {
			get data() {
				const id = getId();
				return id ? h.jobs[id] : undefined;
			},
			get isError() {
				return h.queryError;
			}
		};
	}
}));
vi.mock('$lib/queries/library/LibraryEditionQueries.svelte', () => ({
	getReleaseEditionSearchQuery: () => ({
		data: {
			title_query: 'Local Signals',
			artist_query: 'Signal Artist',
			items: [],
			total: 0,
			offset: 0,
			limit: 12
		},
		isLoading: false,
		isFetching: false,
		isError: false,
		refetch: vi.fn()
	})
}));
vi.mock('$lib/queries/library/LibraryCatalogMutations.svelte', () => ({
	reidentifyLibraryAlbum: () => ({ mutateAsync: h.start, isPending: false, isError: false }),
	reenableAlbumManagement: () => ({ mutateAsync: vi.fn(), isPending: false }),
	selectReidentificationCandidate: () => ({
		mutateAsync: h.select,
		isPending: false,
		get isError() {
			return h.selectError !== null;
		},
		get error() {
			return h.selectError;
		},
		reset: h.resetSelect
	})
}));
vi.mock('$lib/queries/library/EditionConversionQueries.svelte', () => ({
	getEditionConversionQuery: () => ({
		get data() {
			return h.conversionData ?? undefined;
		},
		refetch: h.conversionRefetch
	}),
	createEditionConversionPreflight: () => ({
		mutateAsync: vi.fn(),
		isPending: false,
		reset: vi.fn(),
		get data() {
			return h.conversionPreflightData ?? undefined;
		}
	}),
	createEditionConversionPreview: () => ({ mutateAsync: vi.fn(), isPending: false }),
	startEditionConversion: () => ({ mutateAsync: h.conversionStart, isPending: false }),
	retryEditionConversion: () => ({ mutateAsync: vi.fn(), isPending: false }),
	recheckEditionConversion: () => ({ mutateAsync: vi.fn(), isPending: false }),
	cancelEditionConversion: () => ({ mutateAsync: vi.fn(), isPending: false })
}));
vi.mock('$lib/queries/library/LibraryOperationMutations.svelte', () => ({
	controlLibraryOperation: (action: string) => ({
		mutateAsync: action === 'pause' ? h.pause : action === 'resume' ? h.resume : h.stop
	})
}));

import AlbumIdentificationPanel from './AlbumIdentificationPanel.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	sessionStorage.clear();
	h.jobs = {};
	h.queryError = false;
	h.selectError = null;
	h.conversionData = null;
	h.conversionPreflightData = null;
	h.conversionStart.mockResolvedValue(undefined);
	h.conversionRefetch.mockResolvedValue(undefined);
	h.start.mockResolvedValue(job({ state: 'queued' }));
	h.select.mockResolvedValue(job({ state: 'succeeded' }));
	h.pause.mockResolvedValue(job({ state: 'paused' }));
	h.resume.mockResolvedValue(job({ state: 'running' }));
	h.stop.mockResolvedValue(job({ state: 'stopped' }));
	album.musicbrainz_release_group_id = null;
	album.musicbrainz_release_id = null;
	album.album_identity_state = 'local_only';
	album.identification_status = 'local_metadata';
	album.management_identity_readiness = 'exact_release_required';
	album.mapped_track_count = 0;
	album.active_edition_conversion = null;
	album.track_count = 2;
});

describe('AlbumIdentificationPanel', () => {
	it('shows an accessible management-readiness warning on its trigger', async () => {
		await render(AlbumIdentificationPanel, {
			props: { album, attentionLabel: 'Exact track map required' }
		} as unknown as Parameters<typeof render>[1]);

		const trigger = page.getByRole('button', { name: 'Re-identify…' });
		await expect.element(trigger).toHaveClass(/identification-trigger-warning/);
		await expect.element(page.getByText('Exact track map required')).toBeVisible();
		await expect
			.element(trigger)
			.toHaveAttribute('aria-describedby', 'reidentify-attention-album-1');
	});

	it('starts a durable one-off Local metadata job and keeps it across closure', async () => {
		await render(AlbumIdentificationPanel, {
			props: { album }
		} as unknown as Parameters<typeof render>[1]);
		const opener = page.getByRole('button', { name: 'Re-identify…' });
		await opener.click();
		await expect
			.element(page.getByTestId('identification-workspace'))
			.toHaveClass(/identification-workspace/);
		await expect
			.element(page.getByTestId('identification-scroll-region'))
			.toHaveClass(/identification-scroll-region/);
		expect(
			page
				.getByRole('heading', { name: 'Search exact releases' })
				.element()
				.closest('section')
				?.classList.contains('edition-finder')
		).toBe(true);
		expect(
			page
				.getByRole('heading', { name: 'Search exact releases' })
				.element()
				.closest('.identification-launchpad')
		).not.toBeNull();
		await expect.element(page.getByText(/one-off identification check/)).toBeVisible();
		await expect.element(page.getByText(/job continues/)).toBeVisible();
		await page.getByRole('button', { name: 'Start identification' }).click();
		expect(h.start).toHaveBeenCalledWith({
			albumId: 'album-1',
			expectedAlbumRevision: 5,
			expectedInputRevision: 'input-5',
			oneOffLocalMetadata: true,
			releaseMbid: null
		});
		expect(sessionStorage.getItem('droppedneedle:album-identification:admin-1:album-1')).toBe(
			'job-1'
		);
		await page.getByRole('button', { name: 'Close', exact: true }).click();
		await expect.element(opener).toHaveFocus();
	});

	it('checks only the exact MusicBrainz edition supplied by an administrator', async () => {
		const releaseMbid = '428b6417-8a4d-4a5b-b1a3-8762002167a8';
		await render(AlbumIdentificationPanel, {
			props: { album }
		} as unknown as Parameters<typeof render>[1]);

		await page.getByRole('button', { name: 'Re-identify…' }).click();
		await page.getByText(/Already know the release/).click();
		await page.getByRole('textbox', { name: 'MusicBrainz release UUID or URL' }).fill(releaseMbid);
		await page.getByRole('button', { name: 'Check exact release' }).click();

		expect(h.start).toHaveBeenCalledWith({
			albumId: 'album-1',
			expectedAlbumRevision: 5,
			expectedInputRevision: 'input-5',
			oneOffLocalMetadata: true,
			releaseMbid
		});
		expect(h.select).not.toHaveBeenCalled();
	});

	it('leads with the attached edition before offering replacements', async () => {
		album.musicbrainz_release_group_id = 'group-1';
		album.musicbrainz_release_id = '428b6417-8a4d-4a5b-b1a3-8762002167a8';
		album.album_identity_state = 'release_linked';
		album.identification_status = 'identified';
		album.management_identity_readiness = 'ready';
		album.mapped_track_count = 2;
		await render(AlbumIdentificationPanel, {
			props: { album }
		} as unknown as Parameters<typeof render>[1]);

		await page.getByRole('button', { name: 'Re-identify…' }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Attached exact release' }))
			.toBeVisible();
		await expect.element(page.getByText('Currently attached')).toBeVisible();
		await expect.element(page.getByText('2 of 2 indexed files mapped')).toBeVisible();
		await expect.element(page.getByText('Choose a different edition')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Search', exact: true }))
			.not.toBeInTheDocument();
	});

	it('keeps malformed exact release IDs on the client', async () => {
		await render(AlbumIdentificationPanel, {
			props: { album }
		} as unknown as Parameters<typeof render>[1]);

		await page.getByRole('button', { name: 'Re-identify…' }).click();
		await page.getByText(/Already know the release/).click();
		await page
			.getByRole('textbox', { name: 'MusicBrainz release UUID or URL' })
			.fill('not-a-release');
		await page.getByRole('button', { name: 'Check exact release' }).click();

		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent(/release UUID or canonical release URL/);
		expect(h.start).not.toHaveBeenCalled();
	});

	it('recovers a saved job, projects candidates, and sends the current revision', async () => {
		sessionStorage.setItem('droppedneedle:album-identification:admin-1:album-1', 'job-1');
		h.jobs = { 'job-1': candidateJob };
		await render(AlbumIdentificationPanel, {
			props: { album }
		} as unknown as Parameters<typeof render>[1]);
		await page.getByRole('button', { name: 'Re-identify…' }).click();
		await expect.element(page.getByRole('heading', { name: 'The Right Release' })).toBeVisible();
		await expect.element(page.getByText('Strong evidence')).toBeVisible();
		await page.getByRole('button', { name: 'Use this identity' }).click();
		expect(h.select).toHaveBeenCalledWith({
			jobId: 'job-1',
			expectedRevision: 8,
			candidateKey: 'rg-1:release-1',
			confirmation: false,
			decisionMode: 'exact_release'
		});
		expect(h.start).not.toHaveBeenCalled();
	});

	it('turns an accepted candidate into a terminal identity receipt', async () => {
		const accepted = structuredClone(candidateJob);
		accepted.state = 'succeeded';
		accepted.terminal_code = 'IDENTIFIED';
		accepted.selected_reidentification_candidate_key = 'rg-1:release-1';
		sessionStorage.setItem('droppedneedle:album-identification:admin-1:album-1', 'job-1');
		h.jobs = { 'job-1': accepted };
		await render(AlbumIdentificationPanel, {
			props: { album }
		} as unknown as Parameters<typeof render>[1]);

		await page.getByRole('button', { name: 'Re-identify…' }).click();
		await expect.element(page.getByRole('heading', { name: 'Identity attached' })).toBeVisible();
		await expect.element(page.getByText('Accepted edition')).toBeVisible();
		await expect.element(page.getByRole('heading', { name: 'The Right Release' })).toBeVisible();
		await expect.element(page.getByText('Music filesUnchanged')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Use this identity' }))
			.not.toBeInTheDocument();
		await expect.element(page.getByText(/administrator must confirm/)).not.toBeInTheDocument();
	});

	it('offers exact-edition search when identification finds no candidates', async () => {
		const noCandidates = job({
			state: 'succeeded',
			completed_count: 1,
			terminal_code: 'NO_EXTERNAL_RESULT'
		});
		sessionStorage.setItem('droppedneedle:album-identification:admin-1:album-1', 'job-1');
		h.jobs = { 'job-1': noCandidates };
		await render(AlbumIdentificationPanel, {
			props: { album }
		} as unknown as Parameters<typeof render>[1]);

		await page.getByRole('button', { name: 'Re-identify…' }).click();
		await expect
			.element(page.getByRole('heading', { name: 'No release candidates were found' }))
			.toBeVisible();
		await expect
			.element(page.getByRole('heading', { name: 'Search exact releases' }))
			.toBeVisible();
		await expect
			.element(page.getByRole('heading', { name: 'Identity attached' }))
			.not.toBeInTheDocument();

		// Zero-candidate albums still have an exit: leave the album unmanaged
		// without any candidate evidence.
		const leaveButton = page.getByRole('button', { name: 'Leave unmanaged' });
		await expect.element(leaveButton).toBeVisible();
		await leaveButton.click();
		expect(h.select).toHaveBeenCalledWith({
			jobId: 'job-1',
			expectedRevision: 8,
			candidateKey: '',
			confirmation: true,
			decisionMode: 'leave_unmanaged'
		});
	});

	it('uses the candidate rail to inspect one release dossier at a time', async () => {
		const multipleCandidates = structuredClone(candidateJob);
		const alternate = structuredClone(multipleCandidates.reidentification_candidates[0]);
		alternate.candidate_key = 'rg-2:release-2';
		alternate.evidence.release_group_mbid = 'rg-2';
		alternate.evidence.release_mbid = 'release-2';
		alternate.evidence.album_title = 'Alternate Release';
		alternate.evidence.album_artist_name = 'Another Artist';
		alternate.evidence.score = 0.72;
		multipleCandidates.reidentification_candidates.push(alternate);
		sessionStorage.setItem('droppedneedle:album-identification:admin-1:album-1', 'job-1');
		h.jobs = { 'job-1': multipleCandidates };
		await render(AlbumIdentificationPanel, {
			props: { album }
		} as unknown as Parameters<typeof render>[1]);

		await page.getByRole('button', { name: 'Re-identify…' }).click();
		const alternateButton = page.getByRole('button', { name: /02 Alternate Release/ });
		await alternateButton.click();

		await expect.element(alternateButton).toHaveAttribute('aria-pressed', 'true');
		await expect.element(page.getByRole('heading', { name: 'Alternate Release' })).toBeVisible();
		expect(page.getByTestId('identification-evidence-dossier').element().textContent).toContain(
			'Another Artist'
		);
	});

	it('describes a complete track map with tag-only conflicts accurately', async () => {
		album.track_count = 8;
		const tagConflict = structuredClone(candidateJob);
		const candidate = tagConflict.reidentification_candidates[0];
		candidate.automatic_safe = false;
		candidate.evidence.album_title_classification = 'contradictory';
		candidate.evidence.album_artist_classification = 'contradictory';
		candidate.evidence.reason_code = 'CONTRADICTORY_TRACK_EVIDENCE';
		candidate.evidence.track_evidence = Array.from({ length: 8 }, (_, index) => ({
			local_track_id: `local-${index + 1}`,
			classification: 'supported' as const,
			evidence_kinds: ['recording_id'],
			candidate_track_title: `Track ${index + 1}`,
			candidate_disc_number: 1,
			candidate_track_position: index + 1,
			recording_mbid: `recording-${index + 1}`,
			release_track_mbid: `release-track-${index + 1}`
		}));
		sessionStorage.setItem('droppedneedle:album-identification:admin-1:album-1', 'job-1');
		h.jobs = { 'job-1': tagConflict };
		await render(AlbumIdentificationPanel, {
			props: { album }
		} as unknown as Parameters<typeof render>[1]);

		await page.getByRole('button', { name: 'Re-identify…' }).click();
		await page.getByRole('button', { name: 'Review and use...' }).click();
		const confirmation = page.getByRole('dialog', {
			name: 'Use this identity despite conflicting evidence?'
		});

		await expect.element(confirmation.getByText('All 8 tracks mapped')).toBeVisible();
		await expect
			.element(
				confirmation.getByText('The local album title and album artist do not match this edition')
			)
			.toBeVisible();
		expect(confirmation.element().textContent).not.toContain('conflicting track evidence');
	});

	it('can supersede a ready candidate report with a fresh check', async () => {
		sessionStorage.setItem('droppedneedle:album-identification:admin-1:album-1', 'job-1');
		h.jobs = { 'job-1': candidateJob };
		h.start.mockResolvedValue(job({ id: 'job-2', state: 'queued' }));
		await render(AlbumIdentificationPanel, {
			props: { album }
		} as unknown as Parameters<typeof render>[1]);

		await page.getByRole('button', { name: 'Re-identify…' }).click();
		await page.getByRole('button', { name: 'Check again' }).click();

		expect(h.start).toHaveBeenCalledWith({
			albumId: 'album-1',
			expectedAlbumRevision: 5,
			expectedInputRevision: 'input-5',
			oneOffLocalMetadata: true,
			releaseMbid: null
		});
		expect(sessionStorage.getItem('droppedneedle:album-identification:admin-1:album-1')).toBe(
			'job-2'
		);
	});

	it('can discard a ready evidence check', async () => {
		sessionStorage.setItem('droppedneedle:album-identification:admin-1:album-1', 'job-1');
		h.jobs = { 'job-1': candidateJob };
		await render(AlbumIdentificationPanel, {
			props: { album }
		} as unknown as Parameters<typeof render>[1]);

		await page.getByRole('button', { name: 'Re-identify…' }).click();
		await page.getByRole('button', { name: 'Discard identification evidence' }).click();

		expect(h.stop).toHaveBeenCalledWith({ jobId: 'job-1', expectedRevision: 8 });
	});

	it('controls the persisted job without a fixed-delay refresh', async () => {
		sessionStorage.setItem('droppedneedle:album-identification:admin-1:album-1', 'job-1');
		h.jobs = { 'job-1': job() };
		await render(AlbumIdentificationPanel, {
			props: { album }
		} as unknown as Parameters<typeof render>[1]);
		await page.getByRole('button', { name: 'Re-identify…' }).click();
		await page.getByRole('button', { name: 'Pause identification' }).click();
		await page.getByRole('button', { name: 'Stop identification' }).click();
		expect(h.pause).toHaveBeenCalledWith({ jobId: 'job-1', expectedRevision: 8 });
		expect(h.stop).toHaveBeenCalledWith({ jobId: 'job-1', expectedRevision: 8 });
		expect(AlbumIdentificationPanel.toString()).not.toContain('setTimeout');
	});

	it('offers three honest outcomes when an exact track map is incomplete', async () => {
		const unsafe = structuredClone(candidateJob);
		unsafe.reidentification_candidates[0].automatic_safe = false;
		unsafe.reidentification_candidates[0].evidence.album_title_classification = 'supported';
		unsafe.reidentification_candidates[0].evidence.album_artist_classification = 'unknown';
		unsafe.reidentification_candidates[0].evidence.reason_code = 'CONTRADICTORY';
		unsafe.reidentification_candidates[0].evidence.track_evidence = [
			{
				local_track_id: 'track-supported',
				classification: 'supported',
				evidence_kinds: ['recording_id'],
				candidate_track_title: 'Matching Song',
				candidate_disc_number: 1,
				candidate_track_position: 1,
				recording_mbid: 'recording-supported',
				release_track_mbid: null
			},
			{
				local_track_id: 'track-unknown',
				classification: 'unknown',
				evidence_kinds: [],
				candidate_track_title: null,
				candidate_disc_number: null,
				candidate_track_position: null,
				recording_mbid: null,
				release_track_mbid: null
			},
			{
				local_track_id: 'track-1',
				classification: 'contradictory',
				evidence_kinds: ['recording_id'],
				candidate_track_title: 'Different Song',
				candidate_disc_number: 1,
				candidate_track_position: 1,
				recording_mbid: 'recording-1',
				release_track_mbid: null
			}
		];
		unsafe.reidentification_candidates[0].evidence.unmatched_expected_tracks = ['Missing Song'];
		sessionStorage.setItem('droppedneedle:album-identification:admin-1:album-1', 'job-1');
		h.jobs = { 'job-1': unsafe };
		await render(AlbumIdentificationPanel, {
			props: { album }
		} as unknown as Parameters<typeof render>[1]);

		await page.getByRole('button', { name: 'Re-identify…' }).click();
		await expect.element(page.getByText('Different Song')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Create Custom edition' })).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Prepare conversion' })).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Leave unmanaged' })).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Use conflicting identity' }))
			.not.toBeInTheDocument();
		await page.getByRole('button', { name: 'Create Custom edition' }).click();
		expect(h.select).toHaveBeenCalledWith({
			jobId: 'job-1',
			expectedRevision: 8,
			candidateKey: 'rg-1:release-1',
			confirmation: true,
			decisionMode: 'custom_edition'
		});
	});

	it('blocks a Custom edition when the selected release contradicts the album text', async () => {
		const contradictory = structuredClone(candidateJob);
		contradictory.reidentification_candidates[0].automatic_safe = false;
		contradictory.reidentification_candidates[0].evidence.album_title_classification =
			'contradictory';
		contradictory.reidentification_candidates[0].evidence.track_evidence = [];
		sessionStorage.setItem('droppedneedle:album-identification:admin-1:album-1', 'job-1');
		h.jobs = { 'job-1': contradictory };
		await render(AlbumIdentificationPanel, {
			props: { album }
		} as unknown as Parameters<typeof render>[1]);

		await page.getByRole('button', { name: 'Re-identify…' }).click();
		await expect
			.element(page.getByRole('button', { name: 'Create Custom edition' }))
			.toBeDisabled();
		await expect
			.element(page.getByText('This candidate conflicts with the album title or artist.'))
			.toBeVisible();
	});

	it('restores a sealed conversion token after a page reload', async () => {
		album.active_edition_conversion = {
			job_id: 'conversion-1',
			release_mbid: 'release-1',
			state: 'preflight',
			kept_count: 1,
			acquire_count: 1,
			staged_count: 0,
			failed_count: 0,
			recycle_count: 1,
			row_revision: 3,
			final_preview_job_id: null
		};
		h.conversionData = conversionPreflightStatus;
		sessionStorage.setItem(
			'droppedneedle:edition-conversion-preflight:admin-1:conversion-1',
			'sealed-token'
		);
		await render(AlbumIdentificationPanel, {
			props: { album }
		} as unknown as Parameters<typeof render>[1]);

		await page.getByRole('button', { name: 'Re-identify…' }).click();
		await page.getByRole('button', { name: 'Confirm and acquire missing tracks' }).click();

		expect(h.conversionStart).toHaveBeenCalledWith({
			jobId: 'conversion-1',
			preflightToken: 'sealed-token',
			expectedRevision: 3
		});
		expect(
			sessionStorage.getItem('droppedneedle:edition-conversion-preflight:admin-1:conversion-1')
		).toBeNull();
	});

	it('keeps a failed manual decision visible inside its confirmation dialog', async () => {
		const unsafe = structuredClone(candidateJob);
		unsafe.reidentification_candidates[0].automatic_safe = false;
		unsafe.reidentification_candidates[0].evidence.album_title_classification = 'contradictory';
		h.selectError = new ApiError(409, 'Backend detail', 'STALE_REVISION');
		sessionStorage.setItem('droppedneedle:album-identification:admin-1:album-1', 'job-1');
		h.jobs = { 'job-1': unsafe };
		await render(AlbumIdentificationPanel, {
			props: { album }
		} as unknown as Parameters<typeof render>[1]);

		await page.getByRole('button', { name: 'Re-identify…' }).click();
		await page.getByRole('button', { name: 'Review and use...' }).click();
		const confirmation = page.getByRole('dialog', {
			name: 'Use this identity despite conflicting evidence?'
		});

		expect(h.resetSelect).toHaveBeenCalledOnce();
		await expect.element(confirmation.getByRole('alert')).toHaveTextContent(/the album changed/i);
	});
});

describe('GH-286: attached identity passes the release MBID', () => {
	it('Check attached identity sends the durable release_mbid', async () => {
		h.start.mockClear();
		const attached = {
			...album,
			musicbrainz_release_id: '11111111-2222-4333-8444-555555555555'
		};
		await render(AlbumIdentificationPanel, {
			props: { album: attached }
		} as unknown as Parameters<typeof render>[1]);
		await page.getByRole('button', { name: 'Re-identify…' }).click();
		await page.getByRole('button', { name: 'Check attached identity' }).click();

		expect(h.start).toHaveBeenCalledWith(
			expect.objectContaining({
				releaseMbid: '11111111-2222-4333-8444-555555555555'
			})
		);
	});

	it('Start identification without an attached release sends null', async () => {
		h.start.mockClear();
		await render(AlbumIdentificationPanel, {
			props: { album }
		} as unknown as Parameters<typeof render>[1]);
		await page.getByRole('button', { name: 'Re-identify…' }).click();
		await page.getByRole('button', { name: 'Start identification' }).click();

		expect(h.start).toHaveBeenCalledWith(expect.objectContaining({ releaseMbid: null }));
	});
});
