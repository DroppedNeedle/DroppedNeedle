<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { ListMusic } from 'lucide-svelte';
	import { API } from '$lib/constants';
	import type { PlaylistTrackRequestSettings } from '$lib/api/playlists';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';

	const form = createSettingsForm<PlaylistTrackRequestSettings>({
		loadEndpoint: API.settingsPlaylistTrackRequests(),
		saveEndpoint: API.settingsPlaylistTrackRequests()
	});

	onMount(() => form.load());
	onDestroy(() => form.cleanup());
</script>

<div class="card bg-base-200">
	<div class="card-body">
		<div class="flex items-center gap-2">
			<ListMusic class="h-5 w-5 text-primary" aria-hidden="true" />
			<h2 class="card-title">Playlist requests</h2>
		</div>
		<p class="text-sm text-base-content/60">
			Defaults for a playlist's "Request all tracks" action. Each track is requested on its own and
			sourced from the artist's studio albums where possible. Users can change these per request.
		</p>

		{#if form.loading}
			<div class="space-y-3 pt-2">
				<div class="skeleton h-12 w-full rounded-xl"></div>
			</div>
		{:else if form.data}
			<div class="form-control pt-2">
				<label class="label" for="track-request-mode">
					<span class="label-text font-medium">Mode</span>
				</label>
				<select
					id="track-request-mode"
					class="select select-bordered w-full max-w-xs"
					bind:value={form.data.mode}
				>
					<option value="auto">Auto: request straight away</option>
					<option value="review">Review: show the albums first</option>
				</select>
			</div>

			<div class="form-control pt-2">
				<label class="label" for="track-request-policy">
					<span class="label-text font-medium">Where each track is filed</span>
				</label>
				<select
					id="track-request-policy"
					class="select select-bordered w-full max-w-xs"
					bind:value={form.data.compilation_policy}
				>
					<option value="match_playlist">Match playlist: the album the playlist lists</option>
					<option value="studio">Studio version: the artist's official album</option>
					<option value="flag">Flag: hold tracks where those differ</option>
				</select>
				<p class="mt-1 text-xs text-base-content/50">
					Match playlist mirrors the source, compilations included. Studio version keeps an official
					library and uses a compilation only when nothing official exists.
				</p>
			</div>

			<div class="card-actions justify-end pt-2">
				<button class="btn btn-primary btn-sm" onclick={() => form.save()} disabled={form.saving}>
					{form.saving ? 'Saving…' : 'Save'}
				</button>
			</div>

			{#if form.message}
				<p class="text-sm {form.messageType === 'error' ? 'text-error' : 'text-success'}">
					{form.message}
				</p>
			{/if}
		{/if}
	</div>
</div>
