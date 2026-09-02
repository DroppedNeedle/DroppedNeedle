<script lang="ts">
	import { QUALITY_TIERS, tierIndex } from '../qualityTiers';

	interface Props {
		upgradeAllowed: boolean;
		backgroundScan: boolean;
		qualityCutoff: string;
		minTier: string;
		maxTier: string;
	}

	let {
		upgradeAllowed = $bindable(),
		backgroundScan = $bindable(),
		qualityCutoff = $bindable(),
		minTier,
		maxTier
	}: Props = $props();

	// The cutoff lives inside the accepted band (mirrors the backend clamp):
	// when the band moves past it, follow the nearest edge instead of holding an
	// unsubmittable value.
	function onBandChange() {
		const minIdx = tierIndex(minTier);
		const maxIdx = tierIndex(maxTier);
		if (tierIndex(qualityCutoff) < minIdx) qualityCutoff = minTier;
		else if (tierIndex(qualityCutoff) > maxIdx) qualityCutoff = maxTier;
	}

	$effect(() => {
		void minTier;
		void maxTier;
		onBandChange();
	});
</script>

<div class="flex flex-col gap-3">
	<label class="label min-h-11 cursor-pointer justify-start gap-3 p-0">
		<input type="checkbox" class="toggle toggle-sm toggle-primary" bind:checked={upgradeAllowed} />
		<span class="label-text">Allow automatic upgrades</span>
	</label>
	<p class="text-xs text-base-content/60">
		When on, DroppedNeedle looks for better-quality copies of anything below your cutoff. It never
		downgrades anything.
	</p>
	<label class="label min-h-11 cursor-pointer justify-start gap-3 p-0">
		<input
			type="checkbox"
			class="toggle toggle-sm toggle-primary"
			bind:checked={backgroundScan}
			disabled={!upgradeAllowed}
		/>
		<span class="label-text">Scan for upgrades in the background</span>
	</label>
	<p class="text-xs text-base-content/60">
		A slow periodic sweep that queues a few upgrades at a time. When off, upgrades run only when you
		trigger them.
	</p>
	<label class="form-control max-w-xs">
		<span class="label-text">Upgrade until quality reaches</span>
		<select
			class="select select-bordered select-sm min-h-11"
			bind:value={qualityCutoff}
			disabled={!upgradeAllowed}
			aria-label="Upgrade until quality reaches"
		>
			{#each QUALITY_TIERS as t (t.key)}
				<option
					value={t.key}
					disabled={tierIndex(t.key) < tierIndex(minTier) || tierIndex(t.key) > tierIndex(maxTier)}
				>
					{t.full}
				</option>
			{/each}
		</select>
	</label>
</div>
