import type { components } from '$lib/api/v3/openapi';
import type { DownloadPolicySettings } from '$lib/queries/downloads/DownloadClientsQueries.svelte';

export type QualityRecipeFormat = 'flac' | 'mp3';

export type QualityRecipeQuality =
	| 'below_192'
	| '192_255'
	| '256_319'
	| '320_plus'
	| 'cd'
	| '24_48'
	| '24_96'
	| '24_192'
	| 'hi_res'
	| 'custom';

/** A recipe entry as the editor works on it: the server sends format and quality as plain strings. */
export type QualityRecipeEntry = Omit<
	components['schemas']['QualityRecipeEntry'],
	'format' | 'quality'
> & {
	format: QualityRecipeFormat;
	quality: QualityRecipeQuality;
};

export type QualityRecipeStatus = 'v1' | 'v2' | 'non_convertible' | 'invalid';

export type RecipeMigrationStatus = QualityRecipeStatus | 'projected' | 'current';

export type RecipeDraftEntry = QualityRecipeEntry & { id: string };

export interface RecipeOption {
	quality: Exclude<QualityRecipeQuality, 'custom'>;
	label: string;
	detail: string;
}

export interface PresetFill {
	label: string;
	descriptor: string;
	recipe: readonly QualityRecipeEntry[];
}

export type PresetKey = 'balanced' | 'mp3_only' | 'efficient_192' | 'best_available';

export const FLAC_OPTIONS: readonly RecipeOption[] = [
	{ quality: 'cd', label: 'CD quality', detail: '16-bit / 44.1-48 kHz' },
	{ quality: '24_48', label: '24-bit / 48 kHz', detail: 'High-resolution detail up to 48 kHz' },
	{ quality: '24_96', label: '24-bit / 96 kHz', detail: 'High-resolution detail up to 96 kHz' },
	{ quality: '24_192', label: '24-bit / 192 kHz', detail: 'High-resolution detail up to 192 kHz' },
	{
		quality: 'hi_res',
		label: 'Above 24-bit / 192 kHz',
		detail: 'The highest FLAC detail available'
	}
];

export const MP3_OPTIONS: readonly RecipeOption[] = [
	{ quality: 'below_192', label: 'Below 192', detail: '16-191 kbps' },
	{ quality: '192_255', label: '192-255', detail: '192-255 kbps' },
	{ quality: '256_319', label: '256-319', detail: '256-319 kbps' },
	{ quality: '320_plus', label: '320+', detail: '320 kbps and above' }
];

export const SEARCH_REVIEW_OPTIONS = [
	{
		value: 'when_unsure',
		label: 'When no copy looks right',
		hint: 'Download a copy that looks like the whole album in a quality you accept. Otherwise wait in the queue so you can pick one.'
	},
	{
		value: 'always',
		label: 'Always let me pick',
		hint: 'Every album waits in the queue with the copies found, until you pick one.'
	},
	{
		value: 'off',
		label: 'Never wait',
		hint: 'Download the best copy found, even when it may be incomplete or in the wrong quality.'
	}
] as const;

const MP3_REGIONS: Record<
	Exclude<QualityRecipeQuality, 'custom' | 'cd' | '24_48' | '24_96' | '24_192' | 'hi_res'>,
	{
		min_bitrate_kbps: number | null;
		max_bitrate_kbps: number | null;
	}
> = {
	below_192: { min_bitrate_kbps: 16, max_bitrate_kbps: 191 },
	'192_255': { min_bitrate_kbps: 192, max_bitrate_kbps: 255 },
	'256_319': { min_bitrate_kbps: 256, max_bitrate_kbps: 319 },
	'320_plus': { min_bitrate_kbps: 320, max_bitrate_kbps: null }
};

const LEGACY_TO_RECIPE: Record<string, QualityRecipeQuality> = {
	low: 'below_192',
	mp3_192: '192_255',
	mp3_256: '256_319',
	mp3_320: '320_plus'
};

const RECIPE_TO_LEGACY: Record<
	Exclude<QualityRecipeQuality, 'custom' | 'below_192' | '192_255' | '256_319' | '320_plus'>,
	string
> = {
	cd: 'lossless',
	'24_48': 'lossless',
	'24_96': 'lossless',
	'24_192': 'lossless',
	hi_res: 'lossless'
};

export const PRESETS: Record<PresetKey, PresetFill> = {
	balanced: {
		label: 'Balanced',
		descriptor: 'Starts with CD quality FLAC, then keeps practical MP3 fallbacks.',
		recipe: [
			{ format: 'flac', quality: 'cd' },
			{ format: 'mp3', quality: '320_plus', ...MP3_REGIONS['320_plus'] },
			{ format: 'mp3', quality: '256_319', ...MP3_REGIONS['256_319'] },
			{ format: 'mp3', quality: '192_255', ...MP3_REGIONS['192_255'] }
		]
	},
	mp3_only: {
		label: 'MP3 only',
		descriptor: 'Keeps the recipe compact while avoiding an ultra-low fallback.',
		recipe: [
			{ format: 'mp3', quality: '320_plus', ...MP3_REGIONS['320_plus'] },
			{ format: 'mp3', quality: '256_319', ...MP3_REGIONS['256_319'] },
			{ format: 'mp3', quality: '192_255', ...MP3_REGIONS['192_255'] }
		]
	},
	efficient_192: {
		label: 'Efficient 192+',
		descriptor: 'Starts at 192 kbps and keeps CD-quality FLAC as a final safety net.',
		recipe: [
			{ format: 'mp3', quality: '192_255', ...MP3_REGIONS['192_255'] },
			{ format: 'mp3', quality: '256_319', ...MP3_REGIONS['256_319'] },
			{ format: 'mp3', quality: '320_plus', ...MP3_REGIONS['320_plus'] },
			{ format: 'flac', quality: 'cd' }
		]
	},
	best_available: {
		label: 'Best available',
		descriptor: 'Starts with the highest FLAC detail, then steps down gracefully.',
		recipe: [
			{ format: 'flac', quality: 'hi_res' },
			{ format: 'flac', quality: '24_192' },
			{ format: 'flac', quality: '24_96' },
			{ format: 'flac', quality: '24_48' },
			{ format: 'flac', quality: 'cd' },
			{ format: 'mp3', quality: '320_plus', ...MP3_REGIONS['320_plus'] },
			{ format: 'mp3', quality: '256_319', ...MP3_REGIONS['256_319'] },
			{ format: 'mp3', quality: '192_255', ...MP3_REGIONS['192_255'] },
			{ format: 'mp3', quality: 'below_192', ...MP3_REGIONS['below_192'] }
		]
	}
};

export const PRESET_KEYS: readonly PresetKey[] = [
	'balanced',
	'mp3_only',
	'efficient_192',
	'best_available'
];

function finiteNumber(value: number | string | null | undefined): number | null {
	const parsed = typeof value === 'number' ? value : Number(value);
	return Number.isFinite(parsed) ? parsed : null;
}

function wholeNumber(value: number | string | null | undefined): number | null {
	const parsed = finiteNumber(value);
	return parsed !== null && Number.isInteger(parsed) ? parsed : null;
}

export function recipeEntryKey(
	entry: Pick<QualityRecipeEntry, 'format' | 'quality'> & Partial<QualityRecipeEntry>
): string {
	return [
		entry.format,
		entry.quality,
		entry.min_bitrate_kbps ?? null,
		entry.max_bitrate_kbps ?? null,
		entry.bit_depth ?? null,
		entry.sample_rate_hz ?? null
	].join(':');
}

export function cloneRecipe(recipe: readonly RecipeDraftEntry[]): RecipeDraftEntry[] {
	return recipe.map((entry) => ({ ...entry }));
}

export function recipeWithIds(recipe: readonly QualityRecipeEntry[]): RecipeDraftEntry[] {
	return recipe.map((entry, index) => ({ ...entry, id: `recipe-${index + 1}` }));
}

export function stripRecipeIds(recipe: readonly RecipeDraftEntry[]): QualityRecipeEntry[] {
	return recipe.map(({ id: _id, ...entry }) => ({ ...entry }));
}

export function recipeEntryRange(
	entry: Pick<QualityRecipeEntry, 'format' | 'quality' | 'min_bitrate_kbps' | 'max_bitrate_kbps'>
): { min: number; max: number } | null {
	if (entry.format !== 'mp3') return null;
	if (entry.quality === 'custom') {
		const min = wholeNumber(entry.min_bitrate_kbps);
		const max = wholeNumber(entry.max_bitrate_kbps);
		return min !== null && max !== null ? { min, max } : null;
	}
	const region = MP3_REGIONS[entry.quality as keyof typeof MP3_REGIONS];
	return region && region.min_bitrate_kbps !== null
		? { min: region.min_bitrate_kbps, max: region.max_bitrate_kbps ?? 2048 }
		: null;
}
export function standardRecipeEntry(
	format: 'flac' | 'mp3',
	quality: Exclude<QualityRecipeQuality, 'custom'>
): QualityRecipeEntry {
	if (format === 'mp3') {
		const region = MP3_REGIONS[quality as keyof typeof MP3_REGIONS];
		if (region) return { format, quality, ...region };
	}
	return { format, quality };
}

export function recipeEntryLabel(
	entry: Pick<
		QualityRecipeEntry,
		'format' | 'quality' | 'min_bitrate_kbps' | 'max_bitrate_kbps' | 'bit_depth' | 'sample_rate_hz'
	>
): string {
	if (entry.format === 'flac') {
		if (entry.quality === 'custom') {
			const rate = entry.sample_rate_hz;
			const rateLabel =
				rate === null || rate === undefined
					? '? Hz'
					: rate % 1000 === 0
						? `${rate / 1000} kHz`
						: `${rate} Hz`;
			return `Custom · ${entry.bit_depth ?? '?'}-bit / ${rateLabel}`;
		}
		return FLAC_OPTIONS.find((option) => option.quality === entry.quality)?.label ?? entry.quality;
	}
	if (entry.quality === 'custom') {
		return `Custom · ${entry.min_bitrate_kbps ?? '?'}-${entry.max_bitrate_kbps ?? '?'} kbps`;
	}
	return `${MP3_OPTIONS.find((option) => option.quality === entry.quality)?.label ?? entry.quality} kbps`;
}

export function recipeEntrySummary(
	entry: Pick<
		QualityRecipeEntry,
		'format' | 'quality' | 'min_bitrate_kbps' | 'max_bitrate_kbps' | 'bit_depth' | 'sample_rate_hz'
	>
): string {
	return `${entry.format.toUpperCase()} · ${recipeEntryLabel(entry)}`;
}

export function recipeSummary(recipe: readonly QualityRecipeEntry[]): string {
	if (!recipe.length) return 'No quality recipe configured.';
	return `Try ${recipe.map(recipeEntrySummary).join(' → ')}.`;
}

export function recipeFingerprint(recipe: readonly QualityRecipeEntry[]): string {
	return JSON.stringify(recipe.map((entry) => recipeEntryKey(entry)));
}

export function presetMatches(recipe: readonly QualityRecipeEntry[], preset: PresetFill): boolean {
	return recipeFingerprint(recipe) === recipeFingerprint(preset.recipe);
}
export function customFlacError(
	bitDepth: number | string | null | undefined,
	sampleRate: number | string | null | undefined,
	existing: readonly QualityRecipeEntry[] = [],
	excludeId?: string
): string | null {
	const depth = wholeNumber(bitDepth);
	const rate = wholeNumber(sampleRate);
	if (depth === null || depth < 1 || depth > 64)
		return 'Bit depth must be a whole number from 1 to 64.';
	if (rate === null || rate < 8000 || rate > 768000)
		return 'Sample rate must be a whole number from 8000 to 768000 Hz.';
	const duplicate = existing.find((entry) => {
		const entryId = 'id' in entry && typeof entry.id === 'string' ? entry.id : undefined;
		return (
			entry.format === 'flac' &&
			entry.quality === 'custom' &&
			entry.bit_depth === depth &&
			entry.sample_rate_hz === rate &&
			(excludeId === undefined || entryId !== excludeId)
		);
	});
	if (!duplicate) return null;
	const position = existing.indexOf(duplicate);
	return `This exact FLAC resolution is already at position ${position + 1}.`;
}

export function customMp3Error(
	minBitrate: number | string | null | undefined,
	maxBitrate: number | string | null | undefined,
	existing: readonly QualityRecipeEntry[] = [],
	excludeId?: string
): string | null {
	const min = wholeNumber(minBitrate);
	const max = wholeNumber(maxBitrate);
	if (min === null || max === null) return 'Enter whole-number minimum and maximum bitrates.';
	if (min < 16 || min > 2048 || max < 16 || max > 2048)
		return 'MP3 bitrates must be whole numbers from 16 to 2048 kbps.';
	if (min > max) return 'MP3 minimum must not be above the maximum.';
	const candidate = {
		format: 'mp3' as const,
		quality: 'custom' as const,
		min_bitrate_kbps: min,
		max_bitrate_kbps: max
	};
	const overlap = existing.find((entry) => {
		const entryId = 'id' in entry && typeof entry.id === 'string' ? entry.id : undefined;
		if (excludeId !== undefined && entryId === excludeId) return false;
		const range = recipeEntryRange(entry);
		return (
			range !== null &&
			candidate.min_bitrate_kbps <= range.max &&
			range.min <= candidate.max_bitrate_kbps
		);
	});
	if (overlap) {
		const position = existing.indexOf(overlap);
		return `This range overlaps ${recipeEntryLabel(overlap)}${position >= 0 ? ` at position ${position + 1}` : ''}.`;
	}
	return null;
}
export function standardEntryDuplicate(
	entry: Pick<QualityRecipeEntry, 'format' | 'quality'>,
	existing: readonly RecipeDraftEntry[],
	excludeId?: string
): RecipeDraftEntry | undefined {
	return existing.find(
		(candidate) =>
			candidate.id !== excludeId &&
			candidate.format === entry.format &&
			candidate.quality === entry.quality
	);
}

export function validateRecipeEntry(
	entry: Pick<
		QualityRecipeEntry,
		'format' | 'quality' | 'min_bitrate_kbps' | 'max_bitrate_kbps' | 'bit_depth' | 'sample_rate_hz'
	>,
	existing: readonly RecipeDraftEntry[] = [],
	excludeId?: string
): string | null {
	if (entry.format === 'flac' && entry.quality === 'custom') {
		return customFlacError(entry.bit_depth, entry.sample_rate_hz, existing, excludeId);
	}
	if (entry.format === 'mp3' && entry.quality === 'custom') {
		return customMp3Error(entry.min_bitrate_kbps, entry.max_bitrate_kbps, existing, excludeId);
	}
	const duplicate = standardEntryDuplicate(entry, existing, excludeId);
	return duplicate ? `Already in the recipe at position ${existing.indexOf(duplicate) + 1}.` : null;
}

export function moveRecipeEntry(
	recipe: readonly RecipeDraftEntry[],
	from: number,
	to: number
): RecipeDraftEntry[] {
	if (from < 0 || to < 0 || from >= recipe.length || to >= recipe.length || from === to)
		return cloneRecipe(recipe);
	const next = cloneRecipe(recipe);
	const [entry] = next.splice(from, 1);
	next.splice(to, 0, entry);
	return next;
}

export function migrationStatusFromPolicy(
	policy: DownloadPolicySettings
): { status: RecipeMigrationStatus; message?: string | null } | null {
	if (policy.quality_recipe_status) {
		return {
			status: policy.quality_recipe_status as QualityRecipeStatus,
			message: policy.quality_recipe_error ?? null
		};
	}
	if (policy.quality_recipe_error) {
		return { status: 'invalid', message: policy.quality_recipe_error };
	}
	return null;
}

function legacyOrder(policy: DownloadPolicySettings): string[] {
	const keys = ['low', 'mp3_192', 'mp3_256', 'mp3_320', 'lossless'];
	const low = keys.indexOf(policy.quality_min);
	const high = keys.indexOf(policy.quality_max);
	if (low < 0 || high < 0) return ['mp3_320', 'lossless'];
	return keys.slice(Math.min(low, high), Math.max(low, high) + 1).reverse();
}

type FlacStandardQuality = 'cd' | '24_48' | '24_96' | '24_192' | 'hi_res';

/** A v1 policy's lossless tier projects to every FLAC detail, best first. */
const LOSSLESS_PROJECTION: readonly FlacStandardQuality[] = [
	'hi_res',
	'24_192',
	'24_96',
	'24_48',
	'cd'
];

function migrateLegacyEntries(key: string): QualityRecipeEntry[] | null {
	if (key === 'lossless') {
		return LOSSLESS_PROJECTION.map((quality) => ({
			format: 'flac',
			quality
		}));
	}
	const quality = LEGACY_TO_RECIPE[key];
	if (!quality) return null;
	const region = MP3_REGIONS[quality as keyof typeof MP3_REGIONS];
	return [{ format: 'mp3', quality, ...region }];
}
function statusMessage(status: RecipeMigrationStatus): string {
	if (status === 'non_convertible')
		return 'This saved v1 policy also accepted formats outside FLAC and MP3, so it could not be converted safely.';
	if (status === 'invalid')
		return 'The saved quality recipe is incomplete. Your saved policy is unchanged until you choose a replacement.';
	if (status === 'projected')
		return 'This v1 policy was projected into a FLAC and MP3 recipe for editing; save to adopt it.';
	return '';
}

export interface RecipeMigrationResult {
	recipe: RecipeDraftEntry[];
	status: RecipeMigrationStatus;
	message: string;
}

function validStoredEntry(entry: QualityRecipeEntry): boolean {
	if (entry.format !== 'flac' && entry.format !== 'mp3') return false;
	const options = entry.format === 'flac' ? FLAC_OPTIONS : MP3_OPTIONS;
	if (entry.quality !== 'custom' && !options.some((option) => option.quality === entry.quality)) {
		return false;
	}
	if (entry.format === 'flac') {
		if (entry.min_bitrate_kbps !== null && entry.min_bitrate_kbps !== undefined) return false;
		if (entry.max_bitrate_kbps !== null && entry.max_bitrate_kbps !== undefined) return false;
		if (entry.quality === 'custom') {
			return customFlacError(entry.bit_depth, entry.sample_rate_hz) === null;
		}
		return entry.bit_depth == null && entry.sample_rate_hz == null;
	}
	if (entry.bit_depth !== null && entry.bit_depth !== undefined) return false;
	if (entry.sample_rate_hz !== null && entry.sample_rate_hz !== undefined) return false;
	if (entry.quality === 'custom') {
		return customMp3Error(entry.min_bitrate_kbps, entry.max_bitrate_kbps) === null;
	}
	const canonical = standardRecipeEntry(
		'mp3',
		entry.quality as Exclude<QualityRecipeQuality, 'custom'>
	);
	return (
		(entry.min_bitrate_kbps == null && entry.max_bitrate_kbps == null) ||
		(entry.min_bitrate_kbps === canonical.min_bitrate_kbps &&
			entry.max_bitrate_kbps === canonical.max_bitrate_kbps)
	);
}

export function recipeFromPolicy(policy: DownloadPolicySettings): RecipeMigrationResult {
	const metadata = migrationStatusFromPolicy(policy);
	if (metadata?.status === 'invalid') {
		return {
			recipe: [],
			status: 'invalid',
			message: metadata.message ?? statusMessage('invalid')
		};
	}
	if (metadata?.status === 'non_convertible' || policy.flac_mp3_only === false) {
		return {
			recipe: [],
			status: 'non_convertible',
			message: metadata?.message ?? statusMessage('non_convertible')
		};
	}
	if (policy.quality_recipe && policy.quality_recipe.length > 0) {
		// Each stored entry is checked below before the editor takes it.
		const stored = recipeWithIds(policy.quality_recipe as QualityRecipeEntry[]);
		const hasInvalidEntry = stored.some(
			(entry, index) =>
				!validStoredEntry(entry) || validateRecipeEntry(entry, stored.slice(0, index)) !== null
		);
		if (hasInvalidEntry) {
			return { recipe: [], status: 'invalid', message: statusMessage('invalid') };
		}
		const status = metadata?.status === 'projected' ? 'projected' : 'current';
		return {
			recipe: stored,
			status,
			message: metadata?.message ?? statusMessage(status)
		};
	}
	const legacyEntries = legacyOrder(policy).map((key) => migrateLegacyEntries(key));
	if (legacyEntries.some((entries) => entries === null)) {
		return { recipe: [], status: 'invalid', message: statusMessage('invalid') };
	}
	const recipe = legacyEntries.flatMap((entries) => entries ?? []);
	if (!recipe.length) return { recipe: [], status: 'invalid', message: statusMessage('invalid') };
	return {
		recipe: recipeWithIds(recipe),
		status: 'projected',
		message: statusMessage('projected')
	};
}

const LEGACY_TIER_RANGES = [
	{ key: 'low', min: 16, max: 191 },
	{ key: 'mp3_192', min: 192, max: 255 },
	{ key: 'mp3_256', min: 256, max: 319 },
	{ key: 'mp3_320', min: 320, max: 2048 }
] as const;

type LegacyTier = (typeof LEGACY_TIER_RANGES)[number]['key'] | 'lossless';
const LEGACY_TIER_RANK: Record<LegacyTier, number> = {
	low: 0,
	mp3_192: 1,
	mp3_256: 2,
	mp3_320: 3,
	lossless: 4
};

function legacyTiersForEntry(entry: QualityRecipeEntry): LegacyTier[] {
	if (entry.format === 'flac') return ['lossless'];
	if (entry.format !== 'mp3') return [];

	const standardTier = legacyRecipeKey(entry);
	if (standardTier) return [standardTier as LegacyTier];

	const range = recipeEntryRange(entry);
	if (!range) return [];
	return LEGACY_TIER_RANGES.filter(
		(region) => region.min <= range.max && range.min <= region.max
	).map((region) => region.key);
}

/**
 * Projects every recipe entry onto the legacy five-tier bounds used by the
 * policy API. Custom MP3 ranges can span several tiers, so the projection is
 * based on interval overlap rather than recipe position or just its endpoints.
 */
export function legacyRangeFromRecipe(
	recipe: readonly QualityRecipeEntry[]
): { quality_min: string; quality_max: string } | null {
	const tiers = recipe.flatMap(legacyTiersForEntry);
	if (!tiers.length) return null;

	let minimum: LegacyTier = tiers[0];
	let maximum: LegacyTier = tiers[0];
	for (const tier of tiers.slice(1)) {
		if (LEGACY_TIER_RANK[tier] < LEGACY_TIER_RANK[minimum]) minimum = tier;
		if (LEGACY_TIER_RANK[tier] > LEGACY_TIER_RANK[maximum]) maximum = tier;
	}
	return { quality_min: minimum, quality_max: maximum };
}

export function isRecipeMigrationStatus(
	value: string | null | undefined
): value is RecipeMigrationStatus {
	return (
		value === 'v1' ||
		value === 'v2' ||
		value === 'current' ||
		value === 'projected' ||
		value === 'non_convertible' ||
		value === 'invalid'
	);
}

export function legacyRecipeKey(entry: QualityRecipeEntry): string | null {
	if (entry.format === 'flac')
		return RECIPE_TO_LEGACY[entry.quality as keyof typeof RECIPE_TO_LEGACY] ?? null;
	if (entry.quality === 'below_192') return 'low';
	if (entry.quality === '192_255') return 'mp3_192';
	if (entry.quality === '256_319') return 'mp3_256';
	if (entry.quality === '320_plus') return 'mp3_320';
	return null;
}
