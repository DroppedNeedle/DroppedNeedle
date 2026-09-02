<script lang="ts">
	import {
		getDownloadPolicyQuery,
		saveDownloadPolicy
	} from '$lib/queries/downloads/DownloadClientsQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { untrack } from 'svelte';

	import AdvancedBehaviorSection from './acquisition/AdvancedBehaviorSection.svelte';
	import QualityOrderSection from './acquisition/QualityOrderSection.svelte';
	import {
		legacyRangeFromRecipe,
		recipeFingerprint,
		recipeFromPolicy,
		stripRecipeIds,
		validateRecipeEntry,
		type RecipeDraftEntry
	} from './acquisition/qualityRecipeModel';
	import SourceSelectionSection from './acquisition/SourceSelectionSection.svelte';
	import UpgradesSection from './acquisition/UpgradesSection.svelte';
	import type { DownloadPolicySettings } from '$lib/types';

	const policyQuery = getDownloadPolicyQuery();
	const save = saveDownloadPolicy();

	const LEGACY_TIER_RANK: Record<string, number> = {
		low: 0,
		mp3_192: 1,
		mp3_256: 2,
		mp3_320: 3,
		lossless: 4
	};

	function clampCutoff(cutoff: string, minimum: string, maximum: string): string {
		const cutoffRank = LEGACY_TIER_RANK[cutoff] ?? LEGACY_TIER_RANK[minimum];
		if (cutoffRank < LEGACY_TIER_RANK[minimum]) return minimum;
		if (cutoffRank > LEGACY_TIER_RANK[maximum]) return maximum;
		return cutoff;
	}

	let qualityCutoff = $state('lossless');
	let upgradeAllowed = $state(false);
	let backgroundScan = $state(false);
	let qualityRecipe = $state<RecipeDraftEntry[]>([]);
	let baselineRecipe = $state<RecipeDraftEntry[]>([]);
	let unknownQualityBehavior = $state('allow_as_fallback');
	let baselineUnknownQualityBehavior = $state('allow_as_fallback');
	let migrationStatus = $state<
		'v1' | 'v2' | 'current' | 'projected' | 'non_convertible' | 'invalid' | null
	>(null);
	let migrationMessage = $state<string | null>(null);
	let sourceSelectionMode = $state('source_first');
	let verifyDownloads = $state(true);
	let autoAccept = $state(0.7);
	let manualMin = $state(0.5);
	let maxConcurrent = $state(3);
	let maxFailover = $state(3);
	let preferredQualityWait = $state(15);
	let autoRetryEnabled = $state(true);
	let autoRetryMax = $state(6);
	let usenetMinAge = $state(30);
	let lossyMinBitrateKbps = $state<number | null>(null);
	let lossyMaxBitrateKbps = $state<number | null>(null);
	let baselineFingerprint = $state<string | null>(null);
	let seeded = $state(false);

	const liveLegacyRange = $derived.by(() => legacyRangeFromRecipe(qualityRecipe));
	const qualityMin = $derived(liveLegacyRange?.quality_min ?? 'mp3_320');
	const qualityMax = $derived(liveLegacyRange?.quality_max ?? 'lossless');
	const recipeValidationError = $derived.by(() => {
		for (let index = 0; index < qualityRecipe.length; index += 1) {
			const error = validateRecipeEntry(qualityRecipe[index], qualityRecipe.slice(0, index));
			if (error) return error;
		}
		return null;
	});
	const saveDisabled = $derived(
		save.isPending || qualityRecipe.length === 0 || recipeValidationError !== null
	);

	$effect(() => {
		const range = liveLegacyRange;
		if (!range) return;
		const clamped = clampCutoff(qualityCutoff, range.quality_min, range.quality_max);
		if (clamped !== qualityCutoff) qualityCutoff = clamped;
	});

	$effect(() => {
		const d = policyQuery.data as DownloadPolicySettings | undefined;
		if (!d || seeded) return;
		const migration = recipeFromPolicy(d);
		const unknown = d.unknown_quality_behavior ?? 'allow_as_fallback';
		untrack(() => {
			qualityCutoff = d.quality_cutoff;
			upgradeAllowed = d.upgrade_allowed;
			backgroundScan = d.background_upgrade_scan_enabled;
			qualityRecipe = migration.recipe;
			baselineRecipe = migration.recipe.map((entry) => ({ ...entry }));
			unknownQualityBehavior = unknown;
			baselineUnknownQualityBehavior = unknown;
			migrationStatus = migration.status;
			migrationMessage = migration.message || null;
			verifyDownloads = d.verify_downloads;
			autoAccept = d.preflight_score_auto_accept;
			manualMin = d.preflight_score_manual_min;
			maxConcurrent = d.max_concurrent_downloads;
			maxFailover = d.max_failover_attempts;
			preferredQualityWait = d.preferred_quality_wait_minutes;
			autoRetryEnabled = d.auto_retry_enabled;
			autoRetryMax = d.auto_retry_max_attempts;
			usenetMinAge = d.usenet_min_release_age_minutes;
			lossyMinBitrateKbps = d.lossy_min_bitrate_kbps ?? null;
			lossyMaxBitrateKbps = d.lossy_max_bitrate_kbps ?? null;
			sourceSelectionMode = d.source_selection_mode ?? 'source_first';
			baselineFingerprint = recipeFingerprint(migration.recipe, unknown);
			seeded = true;
		});
	});

	function discardRecipe() {
		qualityRecipe = baselineRecipe.map((entry) => ({ ...entry }));
		unknownQualityBehavior = baselineUnknownQualityBehavior;
	}

	async function onSave() {
		const d = policyQuery.data as DownloadPolicySettings | undefined;
		if (!d) return;
		if (!qualityRecipe.length) {
			toastStore.show({
				message: 'Add at least one quality recipe entry before saving',
				type: 'error'
			});
			return;
		}
		const legacyRange = legacyRangeFromRecipe(qualityRecipe);
		const nextQualityMin = legacyRange?.quality_min ?? qualityMin;
		const nextQualityMax = legacyRange?.quality_max ?? qualityMax;
		const nextCutoff = clampCutoff(qualityCutoff, nextQualityMin, nextQualityMax);
		if (nextCutoff !== qualityCutoff) qualityCutoff = nextCutoff;
		const policy: DownloadPolicySettings = {
			...d,
			quality_min: nextQualityMin,
			quality_max: nextQualityMax,
			quality_cutoff: nextCutoff,
			upgrade_allowed: upgradeAllowed,
			background_upgrade_scan_enabled: backgroundScan,
			flac_mp3_only: true,
			verify_downloads: verifyDownloads,
			preflight_score_auto_accept: autoAccept,
			preflight_score_manual_min: manualMin,
			max_concurrent_downloads: maxConcurrent,
			max_failover_attempts: maxFailover,
			preferred_quality_wait_minutes: preferredQualityWait,
			auto_retry_enabled: autoRetryEnabled,
			auto_retry_max_attempts: autoRetryMax,
			usenet_min_release_age_minutes: usenetMinAge,
			quality_recipe: stripRecipeIds(qualityRecipe),
			quality_preference_order: d.quality_preference_order ?? [],
			preferred_lossy_bitrate_kbps: d.preferred_lossy_bitrate_kbps ?? null,
			lossy_min_bitrate_kbps: lossyMinBitrateKbps,
			lossy_max_bitrate_kbps: lossyMaxBitrateKbps,
			lossless_preference: d.lossless_preference ?? 'highest',
			lossless_max_bit_depth: d.lossless_max_bit_depth ?? null,
			lossless_max_sample_rate_hz: d.lossless_max_sample_rate_hz ?? null,
			unknown_quality_behavior: unknownQualityBehavior,
			source_selection_mode: sourceSelectionMode,
			quality_recipe_status: 'v2',
			quality_recipe_error: null
		};
		try {
			await save.mutateAsync(policy);
			baselineRecipe = qualityRecipe.map((entry) => ({ ...entry }));
			baselineUnknownQualityBehavior = unknownQualityBehavior;
			baselineFingerprint = recipeFingerprint(qualityRecipe, unknownQualityBehavior);
			migrationStatus = 'v2';
			migrationMessage = null;
			toastStore.show({
				message:
					'Acquisition policy saved for new acquisitions; active tasks keep the recipe they started with.',
				type: 'success'
			});
		} catch {
			toastStore.show({
				message: 'Could not save the acquisition policy. Your draft is still here.',
				type: 'error'
			});
		}
	}

	const queryHasError = $derived(Boolean(policyQuery.isError || policyQuery.error));
</script>

{#if queryHasError && !policyQuery.data}
	<section class="alert alert-error" role="alert">
		<div class="min-w-0">
			<h2 class="font-semibold">Could not load acquisition policy</h2>
			<p class="text-sm">Your saved policy was not changed. Retry to try loading it again.</p>
		</div>
		<button
			type="button"
			class="btn btn-sm min-h-11 motion-reduce:transition-none"
			onclick={() => void policyQuery.refetch()}>Retry</button
		>
	</section>
{:else if policyQuery.isPending || !policyQuery.data}
	<section
		class="card border border-base-300 bg-base-100"
		aria-busy="true"
		aria-label="Loading acquisition policy"
	>
		<div class="card-body gap-4">
			<div class="skeleton h-5 w-48"></div>
			<div class="skeleton h-4 w-80 max-w-full"></div>
			<div class="grid gap-3 sm:grid-cols-2">
				<div class="skeleton h-20 w-full"></div>
				<div class="skeleton h-20 w-full"></div>
			</div>
			<div class="skeleton h-32 w-full"></div>
		</div>
	</section>
{:else}
	<section class="card border border-base-300 bg-base-100">
		<div class="card-body min-w-0 gap-4">
			<div>
				<h2 class="font-semibold">Acquisition quality</h2>
				<p class="text-sm text-base-content/70">
					Set which complete-album format and quality DroppedNeedle tries first, then arrange the
					fallbacks. New work uses saved changes; active tasks keep the recipe they started with.
				</p>
			</div>

			<QualityOrderSection
				bind:qualityRecipe
				bind:unknownQualityBehavior
				baseline={baselineFingerprint}
				{baselineRecipe}
				{baselineUnknownQualityBehavior}
				migrationStatus={migrationStatus === 'v1' ? 'projected' : migrationStatus}
				{migrationMessage}
				saving={save.isPending}
				ondiscard={discardRecipe}
			/>

			<div class="divider my-1"></div>

			<div>
				<h3 class="font-medium">Upgrades</h3>
				<UpgradesSection
					bind:upgradeAllowed
					bind:backgroundScan
					bind:qualityCutoff
					minTier={qualityMin}
					maxTier={qualityMax}
				/>
			</div>

			<div>
				<h3 class="font-medium">Source selection</h3>
				<SourceSelectionSection bind:sourceSelectionMode />
			</div>

			<AdvancedBehaviorSection
				bind:autoAccept
				bind:manualMin
				bind:maxConcurrent
				bind:maxFailover
				bind:preferredQualityWait
				bind:autoRetryEnabled
				bind:autoRetryMax
				bind:usenetMinAge
				bind:verifyDownloads
				bind:lossyMinBitrateKbps
				bind:lossyMaxBitrateKbps
			/>
			<div class="flex justify-end border-t border-base-300 pt-4">
				<button
					type="button"
					class="btn btn-primary min-h-11 motion-reduce:transition-none"
					disabled={saveDisabled}
					aria-label="Save acquisition policy"
					onclick={() => void onSave()}
				>
					{save.isPending ? 'Saving acquisition policy…' : 'Save acquisition policy'}
				</button>
			</div>
		</div>
	</section>
{/if}
