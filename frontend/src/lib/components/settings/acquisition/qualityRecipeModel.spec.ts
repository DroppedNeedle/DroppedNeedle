import { describe, expect, it } from 'vitest';

import type { DownloadPolicySettings } from '$lib/types';

import {
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
	quality_max: 'lossless',
	flac_mp3_only: true,
	quality_preference_order: ['lossless', 'mp3_320', 'mp3_256', 'mp3_192'],
	lossless_preference: 'cd'
} as DownloadPolicySettings;

describe('quality recipe model', () => {
	it('projects a FLAC/MP3-only v1 policy into every FLAC bucket at the lossless position', () => {
		const result = recipeFromPolicy(basePolicy);
		expect(result.status).toBe('projected');
		expect(result.recipe.map(({ format, quality }) => ({ format, quality }))).toEqual([
			{ format: 'flac', quality: 'cd' },
			{ format: 'flac', quality: '24_48' },
			{ format: 'flac', quality: '24_96' },
			{ format: 'flac', quality: '24_192' },
			{ format: 'flac', quality: 'hi_res' },
			{ format: 'mp3', quality: '320_plus' },
			{ format: 'mp3', quality: '256_319' },
			{ format: 'mp3', quality: '192_255' }
		]);
		expect(standardRecipeEntry('mp3', 'below_192')).toMatchObject({
			min_bitrate_kbps: 16,
			target_bitrate_kbps: 128,
			max_bitrate_kbps: 191
		});
		expect(standardRecipeEntry('mp3', '192_255')).toMatchObject({
			min_bitrate_kbps: 192,
			target_bitrate_kbps: 192,
			max_bitrate_kbps: 255
		});
		expect(standardRecipeEntry('mp3', '256_319')).toMatchObject({
			min_bitrate_kbps: 256,
			target_bitrate_kbps: 256,
			max_bitrate_kbps: 319
		});
		expect(standardRecipeEntry('mp3', '320_plus')).toMatchObject({
			min_bitrate_kbps: 320,
			target_bitrate_kbps: 320,
			max_bitrate_kbps: null
		});
	});

	it('refuses to project a v1 policy that allowed non-FLAC/MP3 formats', () => {
		const result = recipeFromPolicy({ ...basePolicy, flac_mp3_only: false });
		expect(result.status).toBe('non_convertible');
		expect(result.recipe).toEqual([]);
	});
	it('requires replacement when a saved recipe is non-convertible despite valid entries', () => {
		const result = recipeFromPolicy({
			...basePolicy,
			flac_mp3_only: false,
			quality_recipe_status: 'non_convertible',
			quality_recipe_error: 'The saved policy allows additional formats.',
			quality_recipe: [{ format: 'flac', quality: 'cd' }]
		});
		expect(result.status).toBe('non_convertible');
		expect(result.recipe).toEqual([]);
		expect(result.message).toBe('The saved policy allows additional formats.');
	});

	it('validates custom bounds and inclusive MP3 overlap', () => {
		expect(customFlacError(0, 48_000)).toContain('1 to 64');
		expect(customFlacError(24, 768_001)).toContain('8000 to 768000');
		const standard = standardRecipeEntry('mp3', '192_255');
		expect(customMp3Error(16, 160, 191, [standard])).toBeNull();
		expect(customMp3Error(191, 192, 193, [standard])).toContain('overlaps');
		expect(customMp3Error(16, 320, 200, [])).toContain('in order');
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
					target_bitrate_kbps: 224,
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
					target_bitrate_kbps: 512,
					max_bitrate_kbps: 1024
				}
			])
		).toEqual({ quality_min: 'mp3_320', quality_max: 'mp3_320' });
	});
});
