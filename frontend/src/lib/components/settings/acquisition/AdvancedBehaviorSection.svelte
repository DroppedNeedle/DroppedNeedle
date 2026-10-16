<script lang="ts">
	import Field from '$lib/ui/Field.svelte';
	import Input from '$lib/ui/Input.svelte';
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
		usenetMinAge: number;
		verifyDownloads: boolean;
		lossyMinBitrateKbps: number | null;
		lossyMaxBitrateKbps: number | null;
	}

	let {
		autoAccept = $bindable(),
		manualMin = $bindable(),
		maxConcurrent = $bindable(),
		maxFailover = $bindable(),
		preferredQualityWait = $bindable(),
		autoRetryEnabled = $bindable(),
		autoRetryMax = $bindable(),
		usenetMinAge = $bindable(),
		verifyDownloads = $bindable(),
		lossyMinBitrateKbps = $bindable(),
		lossyMaxBitrateKbps = $bindable()
	}: Props = $props();

	function nullableFrom(raw: string): number | null {
		if (raw === '') return null;
		const n = Number(raw);
		return Number.isFinite(n) ? n : null;
	}

	const boundsEqual = $derived(
		lossyMinBitrateKbps != null &&
			lossyMaxBitrateKbps != null &&
			lossyMinBitrateKbps === lossyMaxBitrateKbps
	);
</script>

<details class="collapse collapse-arrow border border-base-300 bg-base-200/40">
	<summary class="collapse-title text-sm font-medium">Download behavior (advanced)</summary>
	<div class="collapse-content flex flex-col gap-4">
		<fieldset class="fieldset">
			<legend class="fieldset-legend">Lossy rejection bounds</legend>
			<div class="grid gap-3 sm:grid-cols-2">
				<Field label="Reject lossy below (kbps)">
					{#snippet children(control)}
						<Input
							{...control}
							label="Reject lossy audio below this bitrate in kbps"
							type="number"
							size="sm"
							class="w-full"
							min="16"
							max="2048"
							placeholder="none"
							value={lossyMinBitrateKbps ?? ''}
							onchange={(e) => (lossyMinBitrateKbps = nullableFrom(e.currentTarget.value))}
						/>
					{/snippet}
				</Field>
				<Field label="Reject lossy above (kbps)">
					{#snippet children(control)}
						<Input
							{...control}
							label="Reject lossy audio above this bitrate in kbps"
							type="number"
							size="sm"
							class="w-full"
							min="16"
							max="2048"
							placeholder="none"
							value={lossyMaxBitrateKbps ?? ''}
							onchange={(e) => (lossyMaxBitrateKbps = nullableFrom(e.currentTarget.value))}
						/>
					{/snippet}
				</Field>
			</div>
			<p class="text-xs text-base-content/60">
				These are rejection bounds, not targets. Copies outside them are never taken, even as
				fallback.
			</p>
			{#if boundsEqual}
				<p class="text-warning text-xs" role="alert">
					Minimum and maximum are equal: VBR files report an average bitrate, so exactly-bounded
					rejection may miss or mis-accept variable-rate copies.
				</p>
			{/if}
		</fieldset>

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
				help="After this zero-byte wait, try the best lower accepted quality."
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
			<NumberField
				label="Usenet release age before blocklisting (min)"
				size="sm"
				min={0}
				max={1440}
				bind:value={usenetMinAge}
			/>
		</div>

		<ToggleRow
			label="Verify downloads (AcoustID release-group check)"
			bind:checked={verifyDownloads}
		/>
		<ToggleRow label="Auto-retry failed downloads" bind:checked={autoRetryEnabled} />
	</div>
</details>
