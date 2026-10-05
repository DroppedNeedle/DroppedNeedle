import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { DownloadTask, QualityRejectionSummary } from '$lib/types';

const h = vi.hoisted(() => ({
	candidates: [] as unknown[],
	candidateCount: 0,
	topScore: null as number | null,
	summary: null as string | null,
	qualityRejections: {
		outside_policy: 0,
		unknown_rejected: 0,
		not_importable: 0,
		needs_review: 0
	} as QualityRejectionSummary,
	pluginSources: [] as { key: string; display_name: string }[],
	pick: vi.fn(),
	cancel: vi.fn(),
	dismiss: vi.fn()
}));

vi.mock('$lib/queries/downloads/SearchQueries.svelte', () => ({
	getSearchJobQuery: () => ({
		get data() {
			return {
				job_id: 'job-1',
				status: 'reviewing',
				artist_name: 'Yan Qing',
				album_title: 'the arrival',
				candidates: h.candidates,
				candidate_count: h.candidateCount,
				top_score: h.topScore,
				quality_snapshot_summary: h.summary,
				quality_rejections: h.qualityRejections
			};
		}
	}),
	pickSearchCandidate: () => ({ mutate: h.pick, isPending: false }),
	dismissReview: () => ({ mutate: h.dismiss, isPending: false })
}));

vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
	cancelDownload: () => ({ mutate: h.cancel, isPending: false })
}));

vi.mock('$lib/queries/plugins/PluginSourceQueries.svelte', () => ({
	getPluginSourcesQuery: () => ({ data: { sources: h.pluginSources }, isLoading: false })
}));

import ReviewCandidates from './ReviewCandidates.svelte';

async function renderReview(task: DownloadTask) {
	return await render(ReviewCandidates, { props: { task } } as unknown as Parameters<
		typeof render<typeof ReviewCandidates>
	>[1]);
}

function makeTask(): DownloadTask {
	return {
		id: 'task-1',
		search_job_id: 'job-1',
		download_type: 'album',
		album_title: 'the arrival',
		artist_name: 'Yan Qing',
		status: 'queued'
	} as unknown as DownloadTask;
}

function candidate(
	username = 'peer-a',
	tier = 'manual',
	finalScore = 0.6,
	candidateIndex: number | null = null
) {
	return {
		source: 'soulseek',
		username,
		parent_directory: 'dir',
		files: [],
		final_score: finalScore,
		tier,
		candidate_index: candidateIndex
	};
}

describe('ReviewCandidates.svelte', () => {
	beforeEach(() => {
		h.candidates = [candidate()];
		h.candidateCount = 1;
		h.topScore = null;
		h.summary = null;
		h.qualityRejections = {
			outside_policy: 0,
			unknown_rejected: 0,
			not_importable: 0,
			needs_review: 0
		};
		h.pick = vi.fn();
		h.cancel = vi.fn();
		h.dismiss = vi.fn();
		h.pluginSources = [];
	});

	it('keeps rejected results out of the default shortlist', async () => {
		h.candidates = [candidate('recommended'), candidate('weak-match', 'rejected', 0.49)];
		await renderReview(makeTask());

		await expect.element(page.getByText('recommended')).toBeVisible();
		await expect.element(page.getByText('weak-match')).not.toBeInTheDocument();

		await page.getByText('Show all 2 candidates').click();
		await expect.element(page.getByText('weak-match')).toBeVisible();
	});

	it('picks the preserved index after an older review is reranked', async () => {
		h.candidates = [candidate('best-current-match', 'manual', 0.68, 7)];
		await renderReview(makeTask());

		await page.getByRole('button', { name: 'Pick candidate from best-current-match' }).click();

		expect(h.pick).toHaveBeenCalledOnce();
		expect(h.pick.mock.calls[0][0]).toEqual({ jobId: 'job-1', candidate_index: 7 });
	});
	it('locks every pick while a pick mutation is pending', async () => {
		h.candidates = [candidate('first-peer'), candidate('second-peer')];
		await renderReview(makeTask());

		const first = page.getByRole('button', { name: 'Pick candidate from first-peer' });
		const second = page.getByRole('button', { name: 'Pick candidate from second-peer' });
		await first.click();

		await expect.element(first).toBeDisabled();
		await expect.element(second).toBeDisabled();
	});

	it('dismissing rejects the whole review into the watchlist', async () => {
		await renderReview(makeTask());
		await page.getByText('None of these - keep watching').click();
		expect(h.dismiss).toHaveBeenCalledOnce();
		expect(h.dismiss.mock.calls[0][0]).toBe('job-1');
	});
});
