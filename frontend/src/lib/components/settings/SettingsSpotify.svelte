<script lang="ts">
	import { api } from '$lib/api/client';
	import { base, resolve } from '$app/paths';
	import type { SpotifySettings } from '$lib/types';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import { Copy, ExternalLink } from 'lucide-svelte';
	import { onMount, onDestroy } from 'svelte';

	let redirectUri = $state('');
	let copied = $state(false);

	function copyRedirectUri() {
		navigator.clipboard
			.writeText(redirectUri)
			.then(() => {
				copied = true;
				setTimeout(() => (copied = false), 2000);
			})
			.catch(() => {
				/* clipboard unavailable (insecure context / permission denied) - ignore */
			});
	}

	const form = createSettingsForm<SpotifySettings>({
		loadEndpoint: '/api/v1/settings/spotify',
		saveEndpoint: '/api/v1/settings/spotify'
	});

	let showSecret = $state(false);

	onMount(async () => {
		form.load();
		try {
			const data = await api.global.get<{ redirect_uri: string }>(
				'/api/v1/settings/spotify/redirect-uri'
			);
			redirectUri = data.redirect_uri;
		} catch {
			redirectUri = `${window.location.origin}${base}/api/v1/me/connections/spotify/auth/callback`;
		}
	});
	onDestroy(() => form.cleanup());
</script>

<div class="card border border-base-300/50 bg-base-200/60 backdrop-blur-sm">
	<div class="card-body gap-4">
		<div class="flex items-center gap-3">
			<div
				class="flex h-11 w-11 items-center justify-center rounded-xl bg-green-500/10 text-green-400 ring-1 ring-green-500/20"
			>
				<svg class="h-5 w-5" viewBox="0 0 24 24" fill="currentColor">
					<path
						d="M12 2C6.477 2 2 6.477 2 12s4.477 10 10 10 10-4.477 10-10S17.523 2 12 2zm4.586 14.424a.622.622 0 01-.857.207c-2.348-1.435-5.304-1.76-8.785-.964a.622.622 0 11-.277-1.215c3.809-.87 7.076-.496 9.712 1.115a.622.622 0 01.207.857zm1.223-2.723a.778.778 0 01-1.07.257c-2.687-1.652-6.785-2.131-9.965-1.166a.778.778 0 01-.966-.519.778.778 0 01.519-.966c3.632-1.102 8.147-.568 11.225 1.324a.778.778 0 01.257 1.07zm.105-2.835C14.692 8.95 9.375 8.775 6.297 9.71a.934.934 0 11-.543-1.787c3.533-1.073 9.407-.866 13.115 1.337a.934.934 0 01-1.955.606z"
					/>
				</svg>
			</div>
			<div>
				<h2 class="card-title text-2xl">Spotify</h2>
				<p class="text-sm text-base-content/60">App credentials for the whole instance.</p>
			</div>
		</div>

		<div class="rounded-xl border border-info/20 bg-info/5 p-3 text-sm text-base-content/70">
			These are the shared app credentials for one registered Spotify application. Each user links
			<span class="font-medium">their own</span> Spotify account from their
			<a href={resolve('/profile')} class="link link-primary">profile</a> to import their personal playlists.
		</div>

		<div class="rounded-xl border border-warning/20 bg-warning/5 p-3 text-sm text-base-content/70">
			<p class="font-medium text-warning/80">Development mode restriction</p>
			<p class="mt-1">
				Spotify apps in development mode only allow up to 5 explicitly allowlisted users. Each user
				who wants to import playlists must be added by email in your
				<a
					href="https://developer.spotify.com/dashboard"
					target="_blank"
					rel="noopener noreferrer"
					class="link link-primary">Spotify app dashboard</a
				>
				under <span class="font-medium">User Management</span>.
			</p>
		</div>

		{#if form.loading}
			<div class="flex justify-center py-10">
				<span class="loading loading-spinner loading-lg"></span>
			</div>
		{:else if form.data}
			<label class="form-control w-full">
				<span
					class="label-text mb-1 text-xs font-semibold uppercase tracking-wider text-base-content/50"
				>
					Client ID
				</span>
				<input
					type="text"
					class="input input-soft w-full"
					bind:value={form.data.client_id}
					placeholder="Spotify Client ID"
					autocomplete="off"
				/>
			</label>

			<label class="form-control w-full">
				<span
					class="label-text mb-1 text-xs font-semibold uppercase tracking-wider text-base-content/50"
				>
					Client Secret
				</span>
				<div class="relative">
					<input
						type={showSecret ? 'text' : 'password'}
						class="input input-soft w-full pr-16"
						bind:value={form.data.client_secret}
						placeholder="Spotify Client Secret"
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

			<label class="flex cursor-pointer items-center gap-3">
				<input type="checkbox" class="toggle toggle-primary" bind:checked={form.data.enabled} />
				<span class="text-sm">Enable Spotify integration</span>
			</label>

			<div class="rounded-xl border border-base-300/50 bg-base-300/20 p-3">
				<p class="mb-1.5 text-xs font-semibold uppercase tracking-wider text-base-content/50">
					Redirect URI: add this in your Spotify app dashboard
				</p>
				<div class="flex items-center gap-2">
					<code
						class="flex-1 truncate rounded-lg bg-base-100/60 px-2.5 py-1.5 text-xs text-base-content/80"
					>
						{redirectUri}
					</code>
					<button
						type="button"
						class="btn btn-ghost btn-xs shrink-0 rounded-full"
						onclick={copyRedirectUri}
					>
						{#if copied}
							Copied!
						{:else}
							<Copy class="h-3.5 w-3.5" />
						{/if}
					</button>
				</div>
			</div>

			<a
				href="https://developer.spotify.com/dashboard"
				target="_blank"
				rel="noopener noreferrer"
				class="flex w-fit items-center gap-1 text-xs text-base-content/50 transition-colors hover:text-primary"
			>
				<ExternalLink class="h-3 w-3" /> Create a Spotify app to get Client ID + Secret
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
					Save credentials
				</button>
			</div>
		{:else if form.message}
			<div class="alert alert-error"><span>{form.message}</span></div>
		{/if}
	</div>
</div>
