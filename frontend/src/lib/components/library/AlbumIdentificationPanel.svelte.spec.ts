import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { LibraryAlbumDetail } from '$lib/types';
import type { OperationResponse } from '$lib/queries/library/LibraryOperationsTypes';

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
	contribution_id: null,
	contribution_state: null
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
				track_evidence: [],
				unmatched_expected_tracks: []
			}
		}
	]
} as unknown as Partial<OperationResponse>);

const h = vi.hoisted(() => ({
	jobs: {} as Record<string, OperationResponse>,
	getJobId: (() => null) as () => string | null,
	start: vi.fn(),
	select: vi.fn(),
	pause: vi.fn(),
	resume: vi.fn(),
	stop: vi.fn(),
	queryError: false
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
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
vi.mock('$lib/queries/library/LibraryCatalogMutations.svelte', () => ({
	reidentifyLibraryAlbum: () => ({ mutateAsync: h.start, isPending: false, isError: false }),
	selectReidentificationCandidate: () => ({
		mutateAsync: h.select,
		isPending: false,
		isError: false
	})
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
	h.start.mockResolvedValue(job({ state: 'queued' }));
	h.select.mockResolvedValue(job({ state: 'succeeded' }));
	h.pause.mockResolvedValue(job({ state: 'paused' }));
	h.resume.mockResolvedValue(job({ state: 'running' }));
	h.stop.mockResolvedValue(job({ state: 'stopped' }));
});

describe('AlbumIdentificationPanel', () => {
	it('starts a durable one-off Local metadata job and keeps it across closure', async () => {
		render(AlbumIdentificationPanel, {
			props: { album }
		} as unknown as Parameters<typeof render>[1]);
		const opener = page.getByRole('button', { name: 'Re-identify...' });
		await opener.click();
		await expect.element(page.getByText(/one-off identification check/)).toBeVisible();
		await expect.element(page.getByText(/continues on the server if you close/)).toBeVisible();
		await page.getByRole('button', { name: 'Start identification' }).click();
		expect(h.start).toHaveBeenCalledWith({
			albumId: 'album-1',
			expectedAlbumRevision: 5,
			expectedInputRevision: 'input-5',
			oneOffLocalMetadata: true
		});
		expect(sessionStorage.getItem('droppedneedle:album-identification:admin-1:album-1')).toBe(
			'job-1'
		);
		await page.getByRole('button', { name: 'Close', exact: true }).click();
		await expect.element(opener).toHaveFocus();
	});

	it('recovers a saved job, projects candidates, and sends the current revision', async () => {
		sessionStorage.setItem('droppedneedle:album-identification:admin-1:album-1', 'job-1');
		h.jobs = { 'job-1': candidateJob };
		render(AlbumIdentificationPanel, {
			props: { album }
		} as unknown as Parameters<typeof render>[1]);
		await page.getByRole('button', { name: 'Re-identify...' }).click();
		await expect.element(page.getByText('The Right Release')).toBeVisible();
		await expect.element(page.getByText('Strong evidence')).toBeVisible();
		await page.getByRole('button', { name: 'Use this identity' }).click();
		expect(h.select).toHaveBeenCalledWith({
			jobId: 'job-1',
			expectedRevision: 8,
			candidateKey: 'rg-1:release-1',
			confirmation: false
		});
		expect(h.start).not.toHaveBeenCalled();
	});

	it('controls the persisted job without a fixed-delay refresh', async () => {
		sessionStorage.setItem('droppedneedle:album-identification:admin-1:album-1', 'job-1');
		h.jobs = { 'job-1': job() };
		render(AlbumIdentificationPanel, {
			props: { album }
		} as unknown as Parameters<typeof render>[1]);
		await page.getByRole('button', { name: 'Re-identify...' }).click();
		await page.getByRole('button', { name: 'Pause identification' }).click();
		await page.getByRole('button', { name: 'Stop identification' }).click();
		expect(h.pause).toHaveBeenCalledWith({ jobId: 'job-1', expectedRevision: 8 });
		expect(h.stop).toHaveBeenCalledWith({ jobId: 'job-1', expectedRevision: 8 });
		expect(AlbumIdentificationPanel.toString()).not.toContain('setTimeout');
	});

	it('shows the exact conflicts before confirming an unsafe candidate', async () => {
		const unsafe = structuredClone(candidateJob);
		unsafe.reidentification_candidates[0].automatic_safe = false;
		unsafe.reidentification_candidates[0].evidence.album_title_classification = 'contradictory';
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
		render(AlbumIdentificationPanel, {
			props: { album }
		} as unknown as Parameters<typeof render>[1]);

		await page.getByRole('button', { name: 'Re-identify...' }).click();
		await expect.element(page.getByText('Different Song')).toBeVisible();
		await page.getByRole('button', { name: 'Review and use...' }).click();
		const confirmation = page.getByRole('dialog', {
			name: 'Use this identity despite conflicting evidence?'
		});
		await expect
			.element(
				confirmation.getByRole('heading', {
					name: 'Use this identity despite conflicting evidence?'
				})
			)
			.toHaveFocus();
		await expect
			.element(confirmation.getByText('The local evidence conflicts with this release'))
			.toBeVisible();
		await expect
			.element(confirmation.getByRole('heading', { name: 'Failed evidence gates' }))
			.toBeVisible();
		await expect.element(confirmation.getByText('track-1', { exact: true })).toBeVisible();
		await expect.element(confirmation.getByText('track-unknown', { exact: true })).toBeVisible();
		await expect.element(confirmation.getByText('Release group: rg-1')).toBeVisible();
		await expect.element(confirmation.getByText('Release: release-1')).toBeVisible();
		await expect
			.element(confirmation.getByText(/track-supported.*recording-supported/))
			.toBeVisible();
		await expect.element(confirmation.getByText(/durable manual identity/)).toBeVisible();
		await page.getByRole('button', { name: 'Use conflicting identity' }).click();
		expect(h.select).toHaveBeenCalledWith({
			jobId: 'job-1',
			expectedRevision: 8,
			candidateKey: 'rg-1:release-1',
			confirmation: true
		});
	});
});
