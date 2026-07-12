<script lang="ts">
	import type { ListenBrainzConnectionSettings } from '$lib/types';
	import { musicSourceStore, type MusicSource } from '$lib/stores/musicSource';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import { onDestroy } from 'svelte';
	import { fromStore } from 'svelte/store';

	const source = fromStore(musicSourceStore);

	let saving = $state(false);
	let message = $state('');

	const currentSource = $derived(source.current.source);

	const DEFAULT_LISTENBRAINZ_API_URL = 'https://api.listenbrainz.org';

	// endpoint rides on the same instance-wide ListenBrainz settings the backend
	// already persists (username/token stay untouched by loading + saving the
	// full object)
	const endpointForm = createSettingsForm<ListenBrainzConnectionSettings>({
		loadEndpoint: '/api/v1/settings/listenbrainz',
		saveEndpoint: '/api/v1/settings/listenbrainz'
	});

	const isDefaultEndpoint = $derived(
		endpointForm.data ? endpointForm.data.api_url === DEFAULT_LISTENBRAINZ_API_URL : true
	);

	function resetEndpointToDefault() {
		if (endpointForm.data) {
			endpointForm.data.api_url = DEFAULT_LISTENBRAINZ_API_URL;
		}
	}

	onDestroy(() => endpointForm.cleanup());

	async function handleChange(event: Event) {
		const target = event.target as HTMLSelectElement;
		const newSource = target.value as MusicSource;
		if (newSource === currentSource) return;
		saving = true;
		message = '';
		const ok = await musicSourceStore.save(newSource);
		if (ok) {
			message = 'Default music source updated';
			setTimeout(() => {
				message = '';
			}, 5000);
		} else {
			message = "Couldn't save the default music source.";
		}
		saving = false;
	}

	$effect(() => {
		musicSourceStore.load();
	});

	$effect(() => {
		endpointForm.load();
	});
</script>

<div class="card bg-base-200">
	<div class="card-body">
		<h2 class="card-title text-2xl">Primary Music Source</h2>
		<p class="text-base-content/70 mb-4">
			Choose which listening service powers your Home and Discover by default. You can also set this
			from your profile, and switch it on each page.
		</p>

		<fieldset class="fieldset">
			<legend class="fieldset-legend">Default source for your discovery data</legend>
			<select
				class="select select-primary w-full max-w-xs"
				value={currentSource}
				onchange={handleChange}
				disabled={saving}
			>
				<option value="listenbrainz">ListenBrainz</option>
				<option value="lastfm">Last.fm</option>
			</select>
			<p class="label text-base-content/60">
				Your Home and Discover use this source unless you switch it on the page.
			</p>
		</fieldset>

		{#if saving}
			<div class="flex items-center gap-2 mt-2">
				<span class="loading loading-spinner loading-sm"></span>
				<span class="text-sm text-base-content/70">Saving…</span>
			</div>
		{/if}

		{#if message}
			<div class="mt-2">
				<span class="text-sm {message.includes('Couldn') ? 'text-error' : 'text-success'}">
					{message}
				</span>
			</div>
		{/if}
	</div>
</div>

<div class="card bg-base-200 mt-6">
	<div class="card-body">
		<h2 class="card-title text-2xl">ListenBrainz Endpoint</h2>
		<p class="text-base-content/70 mb-4">
			Where charts, recommendations, and scrobbles are sent when ListenBrainz is the source.
		</p>

		{#if endpointForm.loading}
			<div class="flex justify-center items-center py-8">
				<span class="loading loading-spinner loading-lg"></span>
			</div>
		{:else if endpointForm.data}
			<div class="space-y-4">
				<div class="form-control w-full">
					<div class="mb-1 flex items-center justify-between">
						<label
							class="font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
							for="lb-api-url"
						>
							API URL
						</label>
						{#if !isDefaultEndpoint}
							<button
								type="button"
								class="btn btn-ghost btn-xs rounded-full"
								onclick={resetEndpointToDefault}
							>
								Reset to default
							</button>
						{/if}
					</div>
					<input
						id="lb-api-url"
						type="text"
						class="input w-full font-mono text-sm"
						bind:value={endpointForm.data.api_url}
						placeholder={DEFAULT_LISTENBRAINZ_API_URL}
						autocomplete="off"
					/>
					<p class="text-xs text-base-content/50 mt-1 ml-1">
						Self-hosted ListenBrainz supported — leave default for listenbrainz.org
					</p>
				</div>

				{#if endpointForm.message}
					<div
						class="alert"
						class:alert-success={endpointForm.messageType === 'success'}
						class:alert-error={endpointForm.messageType === 'error'}
					>
						<span>{endpointForm.message}</span>
					</div>
				{/if}

				<div class="flex justify-end">
					<button
						type="button"
						class="btn btn-primary"
						onclick={() => void endpointForm.save()}
						disabled={endpointForm.saving}
					>
						{#if endpointForm.saving}
							<span class="loading loading-spinner loading-sm"></span>
						{/if}
						Save Endpoint
					</button>
				</div>
			</div>
		{:else if endpointForm.message}
			<div class="alert alert-error"><span>{endpointForm.message}</span></div>
		{/if}
	</div>
</div>
