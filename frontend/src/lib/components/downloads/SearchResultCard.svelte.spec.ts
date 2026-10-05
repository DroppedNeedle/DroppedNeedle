import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { QualityDecision, ScoredCandidate } from '$lib/types';

import SearchResultCard from './SearchResultCard.svelte';

type RenderOpts = Parameters<typeof render<typeof SearchResultCard>>[1];

async function renderCard(props: Record<string, unknown>) {
	return await render(SearchResultCard, { props } as unknown as RenderOpts);
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
	it('locks the Pick button when disabled (double-pick guard)', async () => {
		const onPick = vi.fn();
		await renderCard({ candidate: makeCandidate(), onPick, disabled: true });
		// a disabled button can't dispatch onclick, so this proves a second pick can't fire
		await expect
			.element(page.getByRole('button', { name: /Pick candidate from alice/ }))
			.toBeDisabled();
		expect(onPick).not.toHaveBeenCalled();
	});

	it('blocks unimportable candidates while outside-policy imports stay reachable via Show all', async () => {
		const onPick = vi.fn();
		await renderCard({ candidate: makeCandidate({ tier: 'rejected' }), onPick });
		const button = page.getByRole('button', {
			name: /Blocked: outside the accepted quality policy/
		});
		await expect.element(button).toBeDisabled();
		await expect.element(button).toHaveTextContent('Unavailable');
		await expect.element(page.getByText('Outside policy', { exact: true })).toBeVisible();
		expect(onPick).not.toHaveBeenCalled();
	});
	it('keeps hard quality rejection unavailable and explains its nested disposition', async () => {
		await renderCard({
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
		await renderCard({
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
});
