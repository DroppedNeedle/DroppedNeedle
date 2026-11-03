import { describe, expect, it } from 'vitest';

import type { DownloadPolicySettings } from '$lib/features/downloads/queries/DownloadClientsQueries.svelte';

import {
	FLAC_OPTIONS,
	customFlacError,
	customMp3Error,
	legacyRangeFromRecipe,
	recipeFromPolicy,
	recipeWithIds,
	standardRecipeEntry,
	validateRecipeEntry
} from './qualityRecipeModel';

const basePolicy = {
	quality_min: 'mp3_192',
	quality_max: 'lossless'
} as DownloadPolicySettings;

describe('quality recipe model', () => {
	it('says the widest FLAC step takes every resolution', () => {
		expect(FLAC_OPTIONS.find((option) => option.quality === 'hi_res')?.label).toBe(
			'Any resolution'
		);
	});

	it('fills the standard MP3 bounds', () => {
		expect(standardRecipeEntry('mp3', 'below_192')).toMatchObject({
			min_bitrate_kbps: 16,
			max_bitrate_kbps: 191
		});
		expect(standardRecipeEntry('mp3', '192_255')).toMatchObject({
			min_bitrate_kbps: 192,
			max_bitrate_kbps: 255
		});
		expect(standardRecipeEntry('mp3', '256_319')).toMatchObject({
			min_bitrate_kbps: 256,
			max_bitrate_kbps: 319
		});
		expect(standardRecipeEntry('mp3', '320_plus')).toMatchObject({
			min_bitrate_kbps: 320,
			max_bitrate_kbps: null
		});
	});

	it('validates custom bounds and inclusive MP3 overlap', () => {
		expect(customFlacError(0, 48_000)).toContain('1 to 64');
		expect(customFlacError(24, 768_001)).toContain('8000 to 768000');
		const standard = standardRecipeEntry('mp3', '192_255');
		expect(customMp3Error(16, 191, [standard])).toBeNull();
		expect(customMp3Error(191, 193, [standard])).toContain('overlaps');
		expect(customMp3Error(320, 200, [])).toContain('not be above');
	});

	it('rejects duplicate exact FLAC custom resolution with its existing position', () => {
		const existing = recipeWithIds([
			{ format: 'flac', quality: 'custom', bit_depth: 24, sample_rate_hz: 96_000 }
		]);
		expect(customFlacError(24, 96_000, existing)).toBe(
			'This exact FLAC resolution is already at position 1.'
		);
		expect(validateRecipeEntry(existing[0], existing, existing[0].id)).toBeNull();
		const loaded = recipeFromPolicy({
			...basePolicy,
			quality_recipe_status: 'v2',
			quality_recipe: [
				{ format: 'flac', quality: 'custom', bit_depth: 24, sample_rate_hz: 96_000 },
				{ format: 'flac', quality: 'custom', bit_depth: 24, sample_rate_hz: 96_000 }
			]
		});
		expect(loaded.status).toBe('invalid');
	});

	it('projects a custom MP3 range across every canonical tier it touches', () => {
		expect(
			legacyRangeFromRecipe([
				{
					format: 'mp3',
					quality: 'custom',
					min_bitrate_kbps: 180,
					max_bitrate_kbps: 400
				}
			])
		).toEqual({ quality_min: 'low', quality_max: 'mp3_320' });
		expect(
			legacyRangeFromRecipe([
				{
					format: 'mp3',
					quality: 'custom',
					min_bitrate_kbps: 400,
					max_bitrate_kbps: 1024
				}
			])
		).toEqual({ quality_min: 'mp3_320', quality_max: 'mp3_320' });
	});
});
