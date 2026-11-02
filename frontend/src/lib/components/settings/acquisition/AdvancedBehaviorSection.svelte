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
		fieldError: (field: string, label: string) => string | null;
	}

	let {
		autoAccept = $bindable(),
		manualMin = $bindable(),
		maxConcurrent = $bindable(),
		maxFailover = $bindable(),
		preferredQualityWait = $bindable(),
		autoRetryEnabled = $bindable(),
		autoRetryMax = $bindable(),
		fieldError
	}: Props = $props();

	const refused = $derived({
		autoAccept: fieldError('preflight_score_auto_accept', 'Auto-accept score'),
		manualMin: fieldError('preflight_score_manual_min', 'Manual-review score'),
		wait: fieldError('preferred_quality_wait_minutes', 'Preferred-quality queue wait'),
		concurrent: fieldError('max_concurrent_downloads', 'Max concurrent downloads'),
		failover: fieldError('max_failover_attempts', 'Max failover attempts'),
		retries: fieldError('auto_retry_max_attempts', 'Auto-retry attempts')
	});

	// A refused field stays out of sight in a closed section, so the section opens.
	let open = $state(false);
	$effect(() => {
		if (Object.values(refused).some(Boolean)) open = true;
	});
</script>

<details class="collapse collapse-arrow border border-base-300 bg-base-200/40" bind:open>
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
				error={refused.autoAccept}
			/>
			<NumberField
				label="Manual-review score (≥)"
				size="sm"
				step={0.05}
				min={0}
				max={1}
				bind:value={manualMin}
				error={refused.manualMin}
			/>
			<NumberField
				label="Preferred-quality queue wait (min)"
				help="After this long in a Soulseek peer's queue with nothing downloaded, try the next copy."
				size="sm"
				min={1}
				max={1440}
				bind:value={preferredQualityWait}
				error={refused.wait}
			/>
			<NumberField
				label="Max concurrent downloads"
				size="sm"
				min={1}
				max={10}
				bind:value={maxConcurrent}
				error={refused.concurrent}
			/>
			<NumberField
				label="Max failover attempts"
				size="sm"
				min={1}
				max={10}
				bind:value={maxFailover}
				error={refused.failover}
			/>
			<NumberField
				label="Auto-retry attempts"
				size="sm"
				min={0}
				max={20}
				bind:value={autoRetryMax}
				error={refused.retries}
				disabled={!autoRetryEnabled}
			/>
		</div>

		<ToggleRow label="Auto-retry failed downloads" bind:checked={autoRetryEnabled} />
	</div>
</details>
