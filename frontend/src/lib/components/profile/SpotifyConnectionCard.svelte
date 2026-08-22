<script lang="ts">
	import { resolve } from '$app/paths';
	import { Loader2 } from 'lucide-svelte';
	import SpotifyIcon from '$lib/components/SpotifyIcon.svelte';
	import { getConnectionsQuery } from '$lib/queries/connections/ConnectionsQuery.svelte';
	import {
		createConnectSpotifyMutation,
		createDisconnectMutation
	} from '$lib/queries/connections/ConnectionsMutations.svelte';

	const connectionsQuery = getConnectionsQuery();
	const spotify = $derived(
		connectionsQuery.data?.connections.find((c) => c.service === 'spotify') ?? null
	);

	const connectMutation = createConnectSpotifyMutation();
	const disconnectMutation = createDisconnectMutation();

	let error = $state<string | null>(null);

	async function connect() {
		error = null;
		try {
			await connectMutation.mutateAsync();
		} catch {
			error = 'Could not start Spotify sign-in. Check that Spotify is configured in Settings.';
		}
	}

	async function disconnect() {
		error = null;
		await disconnectMutation.mutateAsync('spotify');
	}
</script>

<section>
	<h2
		class="mb-4 flex items-center gap-2 text-sm font-semibold uppercase tracking-widest text-base-content/50"
	>
		<SpotifyIcon class="h-4 w-4 text-green-400" />
		Spotify
	</h2>

	<div
		class="space-y-3 rounded-2xl border border-base-300/50 bg-base-200/40 p-4 backdrop-blur-sm sm:p-5"
	>
		<div
			class="crate-card flex items-center justify-between gap-3 rounded-xl border border-base-300/40 bg-base-300/20 p-3"
		>
			<div class="flex min-w-0 items-center gap-3">
				<div
					class="flex h-10 w-10 items-center justify-center rounded-xl bg-green-500/10 text-green-400 ring-1 ring-green-500/20"
				>
					<SpotifyIcon class="h-[1.15rem] w-[1.15rem]" />
				</div>
				<div class="min-w-0">
					<div class="flex items-center gap-2">
						<span class="text-sm font-semibold">Spotify</span>
						<span class="status {spotify ? 'status-success' : 'status-error'} status-sm"></span>
					</div>
					{#if spotify}
						<p class="truncate text-xs text-base-content/50">@{spotify.username || 'linked'}</p>
					{:else}
						<p class="text-xs text-base-content/30">Not connected</p>
					{/if}
				</div>
			</div>

			<div class="shrink-0">
				{#if spotify}
					<button
						type="button"
						class="btn btn-ghost btn-xs rounded-full"
						onclick={disconnect}
						disabled={disconnectMutation.isPending}
					>
						{#if disconnectMutation.isPending}
							<Loader2 class="h-3.5 w-3.5 animate-spin" />
						{/if}
						Disconnect
					</button>
				{:else}
					<button
						type="button"
						class="btn btn-xs gap-1 rounded-full bg-green-600 px-3 text-white shadow-sm transition-transform hover:scale-[1.03] hover:bg-green-500"
						onclick={connect}
						disabled={connectMutation.isPending}
					>
						{#if connectMutation.isPending}
							<Loader2 class="h-3.5 w-3.5 animate-spin" />
						{:else}
							<SpotifyIcon class="h-3.5 w-3.5" />
						{/if}
						Connect
					</button>
				{/if}
			</div>
		</div>

		{#if spotify}
			<p class="px-1 text-xs text-base-content/50">
				Connected. Go to your <a href={resolve('/playlists')} class="link link-primary">Playlists</a
				> to import from Spotify.
			</p>
		{:else}
			<p class="px-1 text-xs text-base-content/40">
				Connect your Spotify account to import your personal playlists into DroppedNeedle.
			</p>
		{/if}

		{#if error}
			<p class="px-1 text-xs text-error">{error}</p>
		{/if}
	</div>
</section>
