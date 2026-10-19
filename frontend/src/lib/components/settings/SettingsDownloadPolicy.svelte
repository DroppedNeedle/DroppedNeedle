<script lang="ts">
	import {
		getDownloadPolicyQuery,
		saveDownloadPolicy
	} from '$lib/queries/downloads/DownloadClientsQueries.svelte';
	import type { DownloadPolicySettings as DownloadPolicyDto } from '$lib/queries/downloads/DownloadClientsQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
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
	import SearchReviewSection from './acquisition/SearchReviewSection.svelte';
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
	let migrationStatus = $state<
		'v1' | 'v2' | 'current' | 'projected' | 'non_convertible' | 'invalid' | null
	>(null);
	let migrationMessage = $state<string | null>(null);
	let searchReview = $state('when_unsure');
	let autoAccept = $state(0.7);
	let manualMin = $state(0.5);
	let maxConcurrent = $state(3);
	let maxFailover = $state(3);
	let preferredQualityWait = $state(15);
	let autoRetryEnabled = $state(true);
	let autoRetryMax = $state(6);
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
		untrack(() => {
			qualityCutoff = d.quality_cutoff;
			upgradeAllowed = d.upgrade_allowed;
			backgroundScan = d.background_upgrade_scan_enabled;
			qualityRecipe = migration.recipe;
			baselineRecipe = migration.recipe.map((entry) => ({ ...entry }));
			migrationStatus = migration.status;
			migrationMessage = migration.message || null;
			autoAccept = d.preflight_score_auto_accept;
			manualMin = d.preflight_score_manual_min;
			maxConcurrent = d.max_concurrent_downloads;
			maxFailover = d.max_failover_attempts;
			preferredQualityWait = d.preferred_quality_wait_minutes;
			autoRetryEnabled = d.auto_retry_enabled;
			autoRetryMax = d.auto_retry_max_attempts;
			searchReview = d.search_review ?? 'when_unsure';
			baselineFingerprint = recipeFingerprint(migration.recipe);
			seeded = true;
		});
	});

	function discardRecipe() {
		qualityRecipe = baselineRecipe.map((entry) => ({ ...entry }));
	}

	async function onSave() {
		const raw = policyQuery.data;
		if (!raw) return;
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
		// The save payload round-trips the full v3 DTO (which carries fields the
		// recipe model does not know, like ignored terms and the recycle bin)
		// with the edited fields overlaid.
		const policy: DownloadPolicyDto = {
			...raw,
			quality_min: nextQualityMin,
			quality_max: nextQualityMax,
			quality_cutoff: nextCutoff,
			upgrade_allowed: upgradeAllowed,
			background_upgrade_scan_enabled: backgroundScan,
			flac_mp3_only: true,
			preflight_score_auto_accept: autoAccept,
			preflight_score_manual_min: manualMin,
			max_concurrent_downloads: maxConcurrent,
			max_failover_attempts: maxFailover,
			preferred_quality_wait_minutes: preferredQualityWait,
			auto_retry_enabled: autoRetryEnabled,
			auto_retry_max_attempts: autoRetryMax,
			quality_recipe: stripRecipeIds(qualityRecipe),
			search_review: searchReview,
			quality_recipe_status: 'v2',
			quality_recipe_error: null
		};
		try {
			await save.mutateAsync(policy);
			baselineRecipe = qualityRecipe.map((entry) => ({ ...entry }));
			baselineFingerprint = recipeFingerprint(qualityRecipe);
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
		<Button size="sm" onclick={() => void policyQuery.refetch()}>Retry</Button>
	</section>
{:else if policyQuery.isPending || !policyQuery.data}
	<Card variant="bordered" as="section" aria-busy="true" aria-label="Loading acquisition policy">
		<div class="flex flex-col gap-4">
			<div class="skeleton h-5 w-48"></div>
			<div class="skeleton h-4 w-80 max-w-full"></div>
			<div class="grid gap-3 sm:grid-cols-2">
				<div class="skeleton h-20 w-full"></div>
				<div class="skeleton h-20 w-full"></div>
			</div>
			<div class="skeleton h-32 w-full"></div>
		</div>
	</Card>
{:else}
	<Card variant="bordered" as="section">
		<div class="flex min-w-0 flex-col gap-4">
			<div>
				<h2 class="font-semibold">Acquisition quality</h2>
				<p class="text-sm text-base-content/70">
					Set which complete-album format and quality DroppedNeedle tries first, then arrange the
					fallbacks. New work uses saved changes; active tasks keep the recipe they started with.
				</p>
			</div>

			<QualityOrderSection
				bind:qualityRecipe
				baseline={baselineFingerprint}
				{baselineRecipe}
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
				<h3 class="font-medium">Picking a copy</h3>
				<p class="text-xs text-base-content/60">
					When an album should wait for you to pick which copy to download.
				</p>
				<SearchReviewSection bind:searchReview />
			</div>

			<AdvancedBehaviorSection
				bind:autoAccept
				bind:manualMin
				bind:maxConcurrent
				bind:maxFailover
				bind:preferredQualityWait
				bind:autoRetryEnabled
				bind:autoRetryMax
			/>
			<div class="flex justify-end border-t border-base-300 pt-4">
				<Button
					variant="primary"
					disabled={saveDisabled}
					aria-label="Save acquisition policy"
					onclick={() => void onSave()}
				>
					{save.isPending ? 'Saving acquisition policy…' : 'Save acquisition policy'}
				</Button>
			</div>
		</div>
	</Card>
{/if}
