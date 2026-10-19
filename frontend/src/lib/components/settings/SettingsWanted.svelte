<script lang="ts">
	import {
		getWantedSettingsQuery,
		saveWantedSettings
	} from '$lib/queries/downloads/DownloadClientsQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type { WantedWatcherSettings } from '$lib/types';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import NumberField from '$lib/ui/NumberField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	const settingsQuery = getWantedSettingsQuery();
	const save = saveWantedSettings();

	let enabled = $state(true);
	let autoDownloadOnFind = $state(true);
	let watchPartialAlbums = $state(true);
	let maxChecksPerSweep = $state(3);
	let seeded = $state(false);

	$effect(() => {
		const d = settingsQuery.data;
		if (d && !seeded) {
			enabled = d.enabled;
			autoDownloadOnFind = d.auto_download_on_find;
			watchPartialAlbums = d.watch_partial_albums;
			maxChecksPerSweep = d.max_checks_per_sweep;
			seeded = true;
		}
	});

	async function onSave() {
		const settings: WantedWatcherSettings = {
			enabled,
			auto_download_on_find: autoDownloadOnFind,
			watch_partial_albums: watchPartialAlbums,
			max_checks_per_sweep: maxChecksPerSweep
		};
		try {
			await save.mutateAsync(settings);
			toastStore.show({ message: 'Wanted watcher settings saved', type: 'success' });
		} catch {
			toastStore.show({ message: 'Could not save wanted watcher settings', type: 'error' });
		}
	}
</script>

<Card variant="bordered" as="section">
	<div class="space-y-4">
		<div>
			<h3 class="font-semibold">Wanted watcher</h3>
			<p class="text-sm text-base-content/70">
				Keeps looking for requests that couldn't be found. Watched albums appear on the requests
				page's Wanted tab.
			</p>
		</div>

		<ToggleRow
			label="Watch failed requests"
			description="Turning this off pauses all checking without losing the watchlist."
			bind:checked={enabled}
		/>
		<ToggleRow
			label="Download automatically when a verified copy appears"
			description="When off, new copies only show up as candidates to review - nothing downloads by itself."
			bind:checked={autoDownloadOnFind}
			disabled={!enabled}
		/>
		<ToggleRow
			label="Also watch albums with missing tracks"
			bind:checked={watchPartialAlbums}
			disabled={!enabled}
		/>

		<div class="grid gap-4 sm:grid-cols-2">
			<NumberField
				label="Albums checked per sweep"
				size="sm"
				min={1}
				max={20}
				bind:value={maxChecksPerSweep}
				disabled={!enabled}
			/>
		</div>

		<div class="flex justify-end">
			<Button variant="primary" size="sm" onclick={onSave} disabled={save.isPending}>Save</Button>
		</div>
	</div>
</Card>
