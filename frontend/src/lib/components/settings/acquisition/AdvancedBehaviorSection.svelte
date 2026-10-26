<script lang="ts">
	import NumberField from '$lib/ui/NumberField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	interface Props {
		autoAccept: number;
		manualMin: number;
		maxConcurrent: number;
		maxFailover: number;
		preferredQualityWait: number;
		autoRetryEnabled: boolean;
		autoRetryMax: number;
	}

	let {
		autoAccept = $bindable(),
		manualMin = $bindable(),
		maxConcurrent = $bindable(),
		maxFailover = $bindable(),
		preferredQualityWait = $bindable(),
		autoRetryEnabled = $bindable(),
		autoRetryMax = $bindable()
	}: Props = $props();
</script>

<details class="collapse collapse-arrow border border-base-300 bg-base-200/40">
	<summary class="collapse-title text-sm font-medium">Download behavior (advanced)</summary>
	<div class="collapse-content flex flex-col gap-4">
		<div class="grid gap-4 sm:grid-cols-2">
			<NumberField
				label="Auto-accept score (≥)"
				size="sm"
				step={0.05}
				min={0}
				max={1}
				bind:value={autoAccept}
			/>
			<NumberField
				label="Manual-review score (≥)"
				size="sm"
				step={0.05}
				min={0}
				max={1}
				bind:value={manualMin}
			/>
			<NumberField
				label="Preferred-quality queue wait (min)"
				help="After this long in a Soulseek peer's queue with nothing downloaded, try the next copy."
				size="sm"
				min={1}
				max={1440}
				bind:value={preferredQualityWait}
			/>
			<NumberField
				label="Max concurrent downloads"
				size="sm"
				min={1}
				max={10}
				bind:value={maxConcurrent}
			/>
			<NumberField
				label="Max failover attempts"
				size="sm"
				min={1}
				max={10}
				bind:value={maxFailover}
			/>
			<NumberField
				label="Auto-retry attempts"
				size="sm"
				min={0}
				max={20}
				bind:value={autoRetryMax}
				disabled={!autoRetryEnabled}
			/>
		</div>

		<ToggleRow label="Auto-retry failed downloads" bind:checked={autoRetryEnabled} />
	</div>
</details>
