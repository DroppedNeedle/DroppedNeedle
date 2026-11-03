<script lang="ts">
	import SelectField from '$lib/ui/SelectField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
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

	const cutoffOptions = $derived(
		QUALITY_TIERS.map((t) => ({
			value: t.key,
			label: t.full,
			disabled: tierIndex(t.key) < tierIndex(minTier) || tierIndex(t.key) > tierIndex(maxTier)
		}))
	);
</script>

<div class="flex flex-col gap-3">
	<ToggleRow
		label="Allow automatic upgrades"
		description="When on, DroppedNeedle looks for better-quality copies of anything below your cutoff. It never downgrades anything."
		bind:checked={upgradeAllowed}
	/>
	<ToggleRow
		label="Scan for upgrades in the background"
		description="A slow periodic sweep that queues a few upgrades at a time. When off, upgrades run only when you trigger them."
		bind:checked={backgroundScan}
		disabled={!upgradeAllowed}
	/>
	<div class="max-w-xs">
		<SelectField
			label="Upgrade until quality reaches"
			bind:value={qualityCutoff}
			options={cutoffOptions}
			disabled={!upgradeAllowed}
		/>
	</div>
</div>
