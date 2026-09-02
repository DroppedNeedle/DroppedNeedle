import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { QualityDecision, ScoredCandidate } from '$lib/types';

import SearchResultCard from './SearchResultCard.svelte';

type RenderOpts = Parameters<typeof render<typeof SearchResultCard>>[1];

function renderCard(props: Record<string, unknown>) {
	return render(SearchResultCard, { props } as unknown as RenderOpts);
}

function makeDecision(overrides: Partial<QualityDecision> = {}): QualityDecision {
	return {
		eligible: true,
		disposition: 'fallback',
		tier: 'manual',
		preference_step: 2,
		quality_recipe_index: 2,
		quality_recipe_entry: { format: 'mp3', quality: '320_plus' },
		lossless_detail_step: null,
		evidence: {
			extension: 'mp3',
			codec_family: 'lossy',
			bitrate_kbps: 320,
			bit_depth: null,
			sample_rate_hz: null,
			total_bytes: 30_000_000,
			audio_file_count: 1,
			mixed_format: false,
			mixed_quality: false,
			certainty: 'partial',
			provenance: 'source_metadata'
		},
		reasons: [],
		summary: 'Fallback quality candidate.',
		...overrides
	};
}

function makeCandidate(overrides: Partial<ScoredCandidate> = {}): ScoredCandidate {
	return {
		username: 'alice',
		parent_directory: 'Radiohead - OK Computer (1997)',
		files: [
			{
				username: 'alice',
				filename: 'Radiohead/OK Computer/01 Airbag.flac',
				parent_directory: 'Radiohead - OK Computer (1997)',
				size: 30_000_000,
				extension: 'flac',
				bitrate: null,
				bit_depth: 16,
				sample_rate: 44100,
				duration: 284,
				has_free_slot: true,
				upload_speed: 2_000_000
			}
		],
		coherence: 0.95,
		file_confidence: 0.9,
		final_score: 0.88,
		tier: 'manual',
		...overrides
	};
}

describe('SearchResultCard.svelte', () => {
	it('renders folder, peer, score percentage and format', async () => {
		renderCard({ candidate: makeCandidate() });
		await expect.element(page.getByText('Radiohead - OK Computer (1997)')).toBeInTheDocument();
		await expect.element(page.getByText('alice')).toBeInTheDocument();
		await expect.element(page.getByText('88%')).toBeInTheDocument();
		await expect.element(page.getByText('FLAC')).toBeInTheDocument();
	});

	it('calls onPick when Pick is clicked', async () => {
		const onPick = vi.fn();
		renderCard({ candidate: makeCandidate(), onPick });
		await page.getByRole('button', { name: /Pick candidate from alice/ }).click();
		expect(onPick).toHaveBeenCalledOnce();
	});

	it('locks the Pick button when disabled (double-pick guard)', async () => {
		const onPick = vi.fn();
		renderCard({ candidate: makeCandidate(), onPick, disabled: true });
		// a disabled button can't dispatch onclick, so this proves a second pick can't fire
		await expect
			.element(page.getByRole('button', { name: /Pick candidate from alice/ }))
			.toBeDisabled();
		expect(onPick).not.toHaveBeenCalled();
	});

	it('exposes the score breakdown via a tooltip', async () => {
		renderCard({ candidate: makeCandidate() });
		await expect.element(page.getByText('88%')).toBeInTheDocument();
		const tip = document.querySelector('[data-tip]');
		expect(tip?.getAttribute('data-tip')).toContain('Coherence');
	});

	it('renders the Usenet variant with indexer, format and size', async () => {
		const usenet = makeCandidate({
			source: 'usenet',
			username: '',
			files: [],
			usenet_release: {
				indexer_id: 'ds',
				indexer_name: 'DrunkenSlug',
				guid: 'g',
				title: 'Radiohead-OK_Computer-FLAC-1997',
				nzb_url: 'https://idx/nzb',
				size_bytes: 2_315_726_631,
				category_ids: [3040],
				grabs: 205,
				files: 113,
				usenet_date: null
			}
		});
		renderCard({ candidate: usenet, albumTitle: 'OK Computer' });
		await expect.element(page.getByText('OK Computer')).toBeInTheDocument(); // clean album heading
		await expect.element(page.getByText('DrunkenSlug')).toBeInTheDocument();
		// the format badge is exactly "FLAC" (the release title also contains "FLAC").
		await expect.element(page.getByText('FLAC', { exact: true })).toBeInTheDocument();
		await expect.element(page.getByText('2.2 GB')).toBeInTheDocument();
	});

	it('shows "unknown" format when an obfuscated title has no quality category', async () => {
		const usenet = makeCandidate({
			source: 'usenet',
			username: '',
			files: [],
			usenet_release: {
				indexer_id: 'ds',
				indexer_name: 'DS',
				guid: 'g',
				title: 'aHR0cHM6 scrambled xQ',
				nzb_url: 'https://idx/nzb',
				size_bytes: 400_000_000,
				category_ids: [],
				grabs: null,
				files: null,
				usenet_date: null
			}
		});
		renderCard({ candidate: usenet, albumTitle: 'Some Album' });
		await expect.element(page.getByText('unknown')).toBeInTheDocument();
	});

	it('labels legacy candidates Within policy and keeps the pick action as Pick anyway', async () => {
		renderCard({ candidate: makeCandidate() });
		await expect.element(page.getByText('Within policy', { exact: true })).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: /Pick candidate from alice/ }))
			.toHaveTextContent('Pick anyway');
	});

	it('marks the top-ranked step as Preferred with a plain Pick button', async () => {
		renderCard({
			candidate: makeCandidate({
				tier: 'auto',
				quality_decision: makeDecision({
					disposition: 'preferred',
					tier: 'auto',
					preference_step: 0,
					quality_recipe_index: 0
				})
			})
		});
		await expect.element(page.getByText('Preferred', { exact: true })).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: /Pick candidate from alice/ }))
			.toHaveTextContent('Pick');
	});

	it('maps computed fallback steps into the Quality chip', async () => {
		renderCard({
			candidate: makeCandidate({
				quality_decision: makeDecision({
					preference_step: 2,
					quality_recipe_index: 2,
					evidence: { ...makeDecision().evidence, certainty: 'partial' }
				})
			})
		});
		await expect.element(page.getByText('Fallback 2', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Unknown', { exact: true })).not.toBeInTheDocument();
		renderCard({
			candidate: makeCandidate({
				quality_decision: makeDecision({
					preference_step: 2,
					quality_recipe_index: 2,
					evidence: { ...makeDecision().evidence, certainty: 'unknown' }
				})
			})
		});
		await expect.element(page.getByText('Unknown', { exact: true })).toBeVisible();
	});

	it('renders nested quality evidence and keeps soft outside-policy manual picks available', async () => {
		const candidate = makeCandidate({
			tier: 'manual',
			quality_decision: makeDecision({
				eligible: false,
				disposition: 'outside_policy',
				tier: 'manual',
				preference_step: 1,
				quality_recipe_index: 1,
				evidence: { ...makeDecision().evidence, certainty: 'inferred' },
				summary: 'This copy is outside the accepted recipe.'
			})
		});
		renderCard({ candidate });
		await expect.element(page.getByText('Outside policy', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Recipe step 2', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Certainty: Inferred', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('Disposition: outside policy', { exact: true }))
			.toBeVisible();
		await expect
			.element(page.getByRole('button', { name: /Pick candidate from alice/ }))
			.toBeEnabled();
	});

	it('blocks unimportable candidates while outside-policy imports stay reachable via Show all', async () => {
		const onPick = vi.fn();
		renderCard({ candidate: makeCandidate({ tier: 'rejected' }), onPick });
		const button = page.getByRole('button', {
			name: /Blocked: outside the accepted quality policy/
		});
		await expect.element(button).toBeDisabled();
		await expect.element(button).toHaveTextContent('Unavailable');
		await expect.element(page.getByText('Outside policy', { exact: true })).toBeVisible();
		expect(onPick).not.toHaveBeenCalled();
	});
	it('keeps hard quality rejection unavailable and explains its nested disposition', async () => {
		renderCard({
			candidate: makeCandidate({
				tier: 'rejected',
				quality_decision: makeDecision({
					eligible: false,
					disposition: 'not_importable',
					tier: null,
					preference_step: null,
					quality_recipe_index: null,
					summary: 'DSD is not an importable audio format.'
				})
			})
		});
		await expect.element(page.getByText('Rejected', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('Disposition: not importable', { exact: true }))
			.toBeVisible();
		await expect
			.element(page.getByRole('button', { name: /Pick candidate from alice/ }))
			.toBeDisabled();
	});
	it('blocks outside-policy candidates with hard quality reasons', async () => {
		const onPick = vi.fn();
		renderCard({
			candidate: makeCandidate({
				quality_decision: makeDecision({
					eligible: false,
					disposition: 'outside_policy',
					tier: 'lossless',
					preference_step: 0,
					quality_recipe_index: 0,
					reasons: ['lossless_resolution_above_maximum'],
					summary: 'FLAC copy exceeds the server limit (24-bit).'
				})
			}),
			onPick
		});

		const button = page.getByRole('button', {
			name: /Pick candidate from alice - FLAC copy exceeds the server limit/
		});
		await expect.element(button).toBeDisabled();
		await expect.element(button).toHaveTextContent('Unavailable');
		expect(onPick).not.toHaveBeenCalled();
	});
	it('uses the generic identity reason for rejected candidates with eligible quality', async () => {
		const onPick = vi.fn();
		renderCard({
			candidate: makeCandidate({
				tier: 'rejected',
				quality_decision: makeDecision({
					eligible: true,
					disposition: 'fallback',
					summary: 'Fallback quality candidate.'
				})
			}),
			onPick
		});

		const button = page.getByRole('button', {
			name: /Pick candidate from alice - Blocked: outside the accepted quality policy\./
		});
		await expect.element(button).toBeDisabled();
		await expect
			.element(button)
			.toHaveAttribute('title', 'Blocked: outside the accepted quality policy.');
		await expect.element(button).not.toHaveAttribute('title', 'Fallback quality candidate.');
	});
});
