<script lang="ts">
	import {
		getWantedSettingsQuery,
		saveWantedSettings
	} from '$lib/queries/downloads/DownloadClientsQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type { WantedWatcherSettings } from '$lib/types';

	const settingsQuery = getWantedSettingsQuery();
	const save = saveWantedSettings();

	let enabled = $state(true);
	let autoDownloadOnFind = $state(true);
	let watchPartialAlbums = $state(true);
	let maxChecksPerSweep = $state(3);
	let dormantAfterDays = $state(365);
	let seeded = $state(false);

	$effect(() => {
		const d = settingsQuery.data;
		if (d && !seeded) {
			enabled = d.enabled;
			autoDownloadOnFind = d.auto_download_on_find;
			watchPartialAlbums = d.watch_partial_albums;
			maxChecksPerSweep = d.max_checks_per_sweep;
			dormantAfterDays = d.dormant_after_days;
			seeded = true;
		}
	});

	async function onSave() {
		const settings: WantedWatcherSettings = {
			enabled,
			auto_download_on_find: autoDownloadOnFind,
			watch_partial_albums: watchPartialAlbums,
			max_checks_per_sweep: maxChecksPerSweep,
			dormant_after_days: dormantAfterDays
		};
		try {
			await save.mutateAsync(settings);
			toastStore.show({ message: 'Wanted watcher settings saved', type: 'success' });
		} catch {
			toastStore.show({ message: 'Could not save wanted watcher settings', type: 'error' });
		}
	}
</script>

<section class="card border border-base-300 bg-base-100">
	<div class="card-body gap-4">
		<div>
			<h3 class="font-semibold">Wanted watcher</h3>
			<p class="text-sm text-base-content/70">
				Keeps looking for requests that couldn't be found. Watched albums appear on the requests
				page's Wanted tab.
			</p>
		</div>

		<label class="label cursor-pointer justify-start gap-3">
			<input type="checkbox" class="toggle toggle-sm toggle-primary" bind:checked={enabled} />
			<span class="label-text">Watch failed requests</span>
		</label>
		<p class="-mt-3 text-xs text-base-content/60">
			Turning this off pauses all checking without losing the watchlist.
		</p>

		<label class="label cursor-pointer justify-start gap-3">
			<input
				type="checkbox"
				class="toggle toggle-sm toggle-primary"
				bind:checked={autoDownloadOnFind}
				disabled={!enabled}
			/>
			<span class="label-text">Download automatically when a verified copy appears</span>
		</label>
		<p class="-mt-3 text-xs text-base-content/60">
			When off, new copies only show up as candidates to review - nothing downloads by itself.
		</p>

		<label class="label cursor-pointer justify-start gap-3">
			<input
				type="checkbox"
				class="toggle toggle-sm toggle-primary"
				bind:checked={watchPartialAlbums}
				disabled={!enabled}
			/>
			<span class="label-text">Also watch albums with missing tracks</span>
		</label>

		<div class="grid gap-4 sm:grid-cols-2">
			<label class="form-control">
				<span class="label-text">Albums checked per sweep</span>
				<input
					type="number"
					min="1"
					max="20"
					class="input input-bordered input-sm"
					bind:value={maxChecksPerSweep}
					disabled={!enabled}
				/>
			</label>
			<label class="form-control">
				<span class="label-text">Go dormant after (days)</span>
				<input
					type="number"
					min="30"
					max="3650"
					class="input input-bordered input-sm"
					bind:value={dormantAfterDays}
					disabled={!enabled}
				/>
			</label>
		</div>

		<div class="flex justify-end">
			<button
				type="button"
				class="btn btn-primary btn-sm"
				onclick={onSave}
				disabled={save.isPending}
			>
				Save
			</button>
		</div>
	</div>
</section>
