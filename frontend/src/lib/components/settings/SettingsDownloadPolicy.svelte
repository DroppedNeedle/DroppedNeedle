<script lang="ts">
	import { TriangleAlert } from 'lucide-svelte';

	import {
		getDownloadPolicyQuery,
		saveDownloadPolicy
	} from '$lib/queries/downloads/DownloadClientsQueries.svelte';
	import type { DownloadPolicySettings as DownloadPolicyView } from '$lib/queries/downloads/DownloadClientsQueries.svelte';
	import Card from '$lib/ui/Card.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	import AdvancedBehaviorSection from './acquisition/AdvancedBehaviorSection.svelte';
	import QualityOrderSection from './acquisition/QualityOrderSection.svelte';
	import {
		cloneRecipe,
		legacyRangeFromRecipe,
		recipeFingerprint,
		recipeFromPolicy,
		stripRecipeIds,
		validateRecipeEntry,
		type RecipeDraftEntry
	} from './acquisition/qualityRecipeModel';
	import SearchReviewSection from './acquisition/SearchReviewSection.svelte';
	import UpgradesSection from './acquisition/UpgradesSection.svelte';

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

	// The stored policy plus the editable recipe. Fields this form does not
	// edit, such as ignored terms and the recycle bin, are sent back as stored.
	type PolicyDraft = DownloadPolicyView & { recipe: RecipeDraftEntry[] };

	function toDraft(view: DownloadPolicyView): PolicyDraft {
		return {
			...view,
			search_review: view.search_review ?? 'when_unsure',
			recipe: recipeFromPolicy(view).recipe
		};
	}

	function toPolicy(draft: PolicyDraft): DownloadPolicyView {
		const { recipe, ...policy } = draft;
		const range = legacyRangeFromRecipe(recipe);
		const qualityMin = range?.quality_min ?? policy.quality_min;
		const qualityMax = range?.quality_max ?? policy.quality_max;
		return {
			...policy,
			quality_min: qualityMin,
			quality_max: qualityMax,
			quality_cutoff: clampCutoff(policy.quality_cutoff, qualityMin, qualityMax),
			flac_mp3_only: true,
			quality_recipe: stripRecipeIds(recipe),
			quality_recipe_status: 'v2',
			quality_recipe_error: null
		};
	}

	const policyQuery = getDownloadPolicyQuery();
	const save = saveDownloadPolicy();

	const section = createSettingsSection<PolicyDraft>(
		{
			get data() {
				return policyQuery.data ? toDraft(policyQuery.data) : undefined;
			},
			get isPending() {
				return policyQuery.isPending;
			},
			get isError() {
				return policyQuery.isError;
			},
			get error() {
				return policyQuery.error;
			},
			refetch: () => policyQuery.refetch()
		},
		{
			mutateAsync: async (draft) => toDraft(await save.mutateAsync(toPolicy(draft))),
			get isPending() {
				return save.isPending;
			}
		},
		{ savedMessage: 'Saved.' }
	);

	const migration = $derived(section.saved ? recipeFromPolicy(section.saved) : null);
	const baselineRecipe = $derived(section.saved?.recipe ?? []);

	// A stored policy that is not in the shape this form saves (a projected v1
	// recipe, or a quality range the recipe no longer implies) can be saved as is.
	const needsRewrite = $derived.by(() => {
		const saved = section.saved;
		if (!saved) return false;
		if (saved.quality_recipe_status !== 'v2') return true;
		const range = legacyRangeFromRecipe(saved.recipe);
		return (
			range !== null &&
			(range.quality_min !== saved.quality_min || range.quality_max !== saved.quality_max)
		);
	});

	function recipeError(recipe: RecipeDraftEntry[]): string | null {
		for (let index = 0; index < recipe.length; index += 1) {
			const error = validateRecipeEntry(recipe[index], recipe.slice(0, index));
			if (error) return error;
		}
		return null;
	}

	// Keeps the upgrade cutoff inside the range the recipe covers.
	$effect(() => {
		const draft = section.draft;
		if (!draft) return;
		const range = legacyRangeFromRecipe(draft.recipe);
		if (!range) return;
		const clamped = clampCutoff(draft.quality_cutoff, range.quality_min, range.quality_max);
		if (clamped !== draft.quality_cutoff) draft.quality_cutoff = clamped;
	});
</script>

<QueryBoundary query={section} what="the acquisition policy">
	{#snippet loading()}
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
	{/snippet}
	{#snippet children(draft)}
		{@const range = legacyRangeFromRecipe(draft.recipe)}
		<Card variant="bordered" as="section">
			<div class="flex min-w-0 flex-col gap-4">
				<div>
					<h2 class="font-semibold">Acquisition quality</h2>
					<p class="text-sm text-base-content/70">
						Set which complete-album format and quality DroppedNeedle tries first, then arrange the
						fallbacks. New work uses saved changes.
					</p>
				</div>

				{#if section.saved?.recycle_bin_warning}
					<div class="alert alert-warning items-start text-sm" role="status">
						<TriangleAlert class="mt-0.5 h-5 w-5" aria-hidden="true" />
						<p class="min-w-0 flex-1">{section.saved.recycle_bin_warning}</p>
					</div>
				{/if}

				<QualityOrderSection
					bind:qualityRecipe={draft.recipe}
					baseline={recipeFingerprint(baselineRecipe)}
					{baselineRecipe}
					migrationStatus={migration?.status === 'v1' ? 'projected' : (migration?.status ?? null)}
					migrationMessage={migration?.message || null}
					saving={section.saving}
					ondiscard={() => (draft.recipe = cloneRecipe(baselineRecipe))}
				/>

				<div class="divider my-1"></div>

				<div>
					<h3 class="font-medium">Upgrades</h3>
					<UpgradesSection
						bind:upgradeAllowed={draft.upgrade_allowed}
						bind:backgroundScan={draft.background_upgrade_scan_enabled}
						bind:qualityCutoff={draft.quality_cutoff}
						minTier={range?.quality_min ?? 'mp3_320'}
						maxTier={range?.quality_max ?? 'lossless'}
					/>
				</div>

				<div>
					<h3 class="font-medium">Picking a copy</h3>
					<p class="text-xs text-base-content/60">
						When an album should wait for you to pick which copy to download.
					</p>
					<SearchReviewSection bind:searchReview={draft.search_review} />
				</div>

				<AdvancedBehaviorSection
					bind:autoAccept={draft.preflight_score_auto_accept}
					bind:manualMin={draft.preflight_score_manual_min}
					bind:maxConcurrent={draft.max_concurrent_downloads}
					bind:maxFailover={draft.max_failover_attempts}
					bind:preferredQualityWait={draft.preferred_quality_wait_minutes}
					bind:autoRetryEnabled={draft.auto_retry_enabled}
					bind:autoRetryMax={draft.auto_retry_max_attempts}
				/>
				<!-- The recipe editor above has its own Discard, so the bar shows none. -->
				<div class="border-t border-base-300 pt-4">
					<SaveBar
						{...section.bar}
						dirty={section.dirty || needsRewrite}
						ondiscard={undefined}
						disabled={draft.recipe.length === 0 || recipeError(draft.recipe) !== null}
						saveLabel="Save acquisition policy"
					/>
				</div>
			</div>
		</Card>
	{/snippet}
</QueryBoundary>
