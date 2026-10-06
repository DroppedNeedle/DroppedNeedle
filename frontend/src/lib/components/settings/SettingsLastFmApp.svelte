<script lang="ts">
	import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';
	import type { LastFmConnectionSettingsResponse } from '$lib/types';
	import { withBasePath } from '$lib/utils/basePath';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import { Radio, ExternalLink } from 'lucide-svelte';
	import { onMount, onDestroy } from 'svelte';

	// The instance switch and the server's Last.fm app key pair. Users link
	// their own account and pick scrobble toggles in the profile's "Scrobbling
	// & Discovery" card. A user's own key pair, when saved, wins over this one
	// for that user. The secrets come back masked; saving the mask keeps the
	// stored value.
	const form = createSettingsForm<LastFmConnectionSettingsResponse>({
		loadEndpoint: SETTINGS_ENDPOINTS.lastfm(),
		saveEndpoint: SETTINGS_ENDPOINTS.lastfm()
	});

	let showSecret = $state(false);

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
				<p class="text-sm text-base-content/60">App credentials for the whole server.</p>
			</div>
		</div>

		<div class="rounded-xl border border-info/20 bg-info/5 p-3 text-sm text-base-content/70">
			Register one Last.fm API application and save its key and shared secret here; every user links
			with it. Each user then links <span class="font-medium">their own</span> Last.fm account and
			toggles scrobbling from their
			<a href={withBasePath('/profile')} class="link link-primary">profile</a>. Without a key here,
			Connect asks each user for their own key and secret; a user's own pair always wins over this
			one.
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

			<label class="form-control w-full">
				<span
					class="label-text mb-1 text-xs font-semibold uppercase tracking-wider text-base-content/50"
				>
					API key
				</span>
				<input
					type="text"
					class="input input-soft w-full"
					bind:value={form.data.api_key}
					placeholder="Last.fm API key"
					autocomplete="off"
				/>
			</label>

			<label class="form-control w-full">
				<span
					class="label-text mb-1 text-xs font-semibold uppercase tracking-wider text-base-content/50"
				>
					Shared secret
				</span>
				<div class="relative">
					<input
						type={showSecret ? 'text' : 'password'}
						class="input input-soft w-full pr-16"
						bind:value={form.data.shared_secret}
						placeholder="Shared secret"
						autocomplete="off"
					/>
					<button
						type="button"
						class="btn btn-ghost btn-xs absolute right-2 top-1/2 -translate-y-1/2 rounded-full"
						onclick={() => (showSecret = !showSecret)}
					>
						{showSecret ? 'Hide' : 'Show'}
					</button>
				</div>
			</label>

			<a
				href="https://www.last.fm/api/account/create"
				target="_blank"
				rel="noopener noreferrer"
				class="flex w-fit items-center gap-1 text-xs text-base-content/50 transition-colors hover:text-primary"
			>
				<ExternalLink class="h-3 w-3" /> Register an app to get a key and secret
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
