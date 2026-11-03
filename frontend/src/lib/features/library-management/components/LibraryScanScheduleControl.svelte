<script lang="ts">
	import Button from '$lib/ui/Button.svelte';
	import Field from '$lib/ui/Field.svelte';
	import Input from '$lib/ui/Input.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import { getLibraryScanScheduleQuery } from '$lib/features/library/queries/LibraryQueries.svelte';
	import { createSaveLibraryScanScheduleMutation } from '$lib/features/library/queries/LibraryMutations.svelte';
	import { SCAN_FREQUENCY_OPTIONS } from '$lib/features/library-management/components/scanFrequency';
	import { toastStore } from '$lib/stores/toast';
	import type { ScanFrequency } from '$lib/types';

	const scheduleQuery = getLibraryScanScheduleQuery();
	const save = createSaveLibraryScanScheduleMutation();

	let frequency = $state<ScanFrequency>('24hr');
	let dailyTime = $state('03:00');
	let seeded = $state(false);

	$effect(() => {
		const d = scheduleQuery.data;
		if (d && !seeded) {
			frequency = d.scan_frequency;
			dailyTime = d.daily_scan_time ?? '03:00';
			seeded = true;
		}
	});

	const isDaily = $derived(frequency === 'daily');
	const timezone = $derived(scheduleQuery.data?.server_timezone ?? '');
	const timeChanged = $derived(isDaily && scheduleQuery.data?.daily_scan_time !== dailyTime);
	const dirty = $derived(
		seeded && (scheduleQuery.data?.scan_frequency !== frequency || timeChanged)
	);
	const lastScan = $derived(scheduleQuery.data?.last_scan_at ?? null);

	async function handleSave() {
		try {
			await save.mutateAsync({
				scan_frequency: frequency,
				daily_scan_time: dailyTime
			});
			toastStore.show({ message: 'Scan schedule saved', type: 'success' });
		} catch (e) {
			toastStore.show({
				message: e instanceof Error ? e.message : 'Failed to save schedule',
				type: 'error'
			});
		}
	}

	function formatLastScan(ts: number | null): string {
		if (!ts) return 'Never';
		return new Date(ts * 1000).toLocaleString();
	}
</script>

<div class="space-y-3">
	<div class="flex flex-wrap items-end gap-3">
		<SelectField
			label="Scheduled scan frequency"
			bind:value={frequency}
			options={SCAN_FREQUENCY_OPTIONS}
		/>
		{#if isDaily}
			<Field label="Time of day">
				{#snippet children(control)}
					<Input {...control} type="time" label="Time of day" bind:value={dailyTime} />
				{/snippet}
			</Field>
		{/if}
		<Button variant="primary" loading={save.isPending} disabled={!dirty} onclick={handleSave}
			>Save</Button
		>
	</div>
	{#if isDaily}
		<p class="text-xs text-base-content/60">
			Scans every day at {dailyTime}{timezone ? ` (${timezone})` : ''}. If the app is offline at
			that time, it scans the next time it starts.
		</p>
	{/if}
	{#if lastScan != null}
		<p class="text-xs text-base-content/60">Last scan: {formatLastScan(lastScan)}</p>
	{/if}
</div>
