<script lang="ts">
	import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';
	import type { LastFmConnectionSettingsResponse } from '$lib/types';
	import { withBasePath } from '$lib/utils/basePath';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import { Radio, ExternalLink } from 'lucide-svelte';
	import { onMount, onDestroy } from 'svelte';

	// The instance switch only: each user enters their own Last.fm API key and
	// secret, links their account and picks scrobble toggles in the profile's
	// "Scrobbling & Discovery" card.
	const form = createSettingsForm<LastFmConnectionSettingsResponse>({
		loadEndpoint: SETTINGS_ENDPOINTS.lastfm(),
		saveEndpoint: SETTINGS_ENDPOINTS.lastfm()
	});

	onMount(() => {
		form.load();
	});
	onDestroy(() => form.cleanup());
</script>

<div class="card border border-base-300/50 bg-base-200/60 backdrop-blur-sm">
	<div class="card-body gap-4">
		<div class="flex items-center gap-3">
			<div
				class="flex h-11 w-11 items-center justify-center rounded-xl bg-red-500/10 text-red-400 ring-1 ring-red-500/20"
			>
				<Radio class="h-5 w-5" />
			</div>
			<div>
				<h2 class="card-title text-2xl">Last.fm</h2>
				<p class="text-sm text-base-content/60">Turn Last.fm on for this server.</p>
			</div>
		</div>

		<div class="rounded-xl border border-info/20 bg-info/5 p-3 text-sm text-base-content/70">
			Each user enters <span class="font-medium">their own</span> Last.fm API key and secret, links
			their account and toggles scrobbling from their
			<a href={withBasePath('/profile')} class="link link-primary">profile</a>.
		</div>

		{#if form.loading}
			<div class="flex justify-center py-10">
				<span class="loading loading-spinner loading-lg"></span>
			</div>
		{:else if form.data}
			<label class="label cursor-pointer justify-start gap-3">
				<input type="checkbox" class="toggle toggle-primary" bind:checked={form.data.enabled} />
				<span class="label-text">Allow Last.fm scrobbling and linking</span>
			</label>

			<a
				href="https://www.last.fm/api/account/create"
				target="_blank"
				rel="noopener noreferrer"
				class="flex w-fit items-center gap-1 text-xs text-base-content/50 transition-colors hover:text-primary"
			>
				<ExternalLink class="h-3 w-3" /> Where users register an app to get a key and secret
			</a>

			{#if form.message}
				<div
					class="alert"
					class:alert-success={form.messageType === 'success'}
					class:alert-error={form.messageType === 'error'}
				>
					<span>{form.message}</span>
				</div>
			{/if}

			<div class="flex justify-end pt-1">
				<button
					type="button"
					class="btn btn-primary glow-primary-soft gap-2 rounded-full"
					onclick={() => void form.save()}
					disabled={form.saving}
				>
					{#if form.saving}
						<span class="loading loading-spinner loading-sm"></span>
					{/if}
					Save
				</button>
			</div>
		{:else if form.message}
			<div class="alert alert-error"><span>{form.message}</span></div>
		{/if}
	</div>
</div>
