<script lang="ts">
	import Button from '$lib/ui/Button.svelte';
	import SpotifyIcon from '$lib/media/icons/SpotifyIcon.svelte';
	import { getConnectionsQuery } from '$lib/features/profile/queries/ConnectionsQuery.svelte';
	import {
		createConnectSpotifyMutation,
		createDisconnectMutation
	} from '$lib/features/profile/queries/ConnectionsMutations.svelte';
	import { withBasePath } from '$lib/utils/basePath';

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
		try {
			await disconnectMutation.mutateAsync('spotify');
		} catch {
			error = 'Could not disconnect Spotify. Try again.';
		}
	}
</script>

<div class="pf-box flex flex-wrap items-center gap-3 px-4 py-3">
	<span class="sp-icon" aria-hidden="true"><SpotifyIcon class="size-5" /></span>
	<div class="min-w-0 flex-1">
		<p class="flex items-center gap-2 font-semibold">
			Spotify
			<span
				class="status status-sm {spotify ? 'status-success' : 'status-error'}"
				aria-hidden="true"
			></span>
		</p>
		{#if spotify}
			<p class="pf-meta truncate">@{spotify.username || 'linked'}</p>
		{:else}
			<p class="pf-meta">Not connected</p>
		{/if}
	</div>
	{#if spotify}
		<Button
			variant="outline"
			size="display-sm"
			loading={disconnectMutation.isPending}
			onclick={disconnect}
		>
			Disconnect
		</Button>
	{:else}
		<Button
			variant="spotify"
			size="display-sm"
			loading={connectMutation.isPending}
			onclick={connect}
		>
			{#if !connectMutation.isPending}
				<SpotifyIcon class="size-4" />
			{/if}
			Connect
		</Button>
	{/if}
</div>

{#if spotify}
	<p class="mt-2.5 text-sm text-muted">
		Connected. Go to your <a href={withBasePath('/playlists')} class="link link-primary"
			>Playlists</a
		> to import from Spotify.
	</p>
{:else}
	<p class="mt-2.5 text-sm text-muted">
		Connect your Spotify account to import your personal playlists into DroppedNeedle.
	</p>
{/if}

{#if error}
	<p class="mt-2 text-sm text-error">{error}</p>
{/if}

<style>
	.sp-icon {
		display: grid;
		place-items: center;
		width: 2.75rem;
		height: 2.75rem;
		border-radius: 0.5rem;
		background-color: var(--color-base-300);
		color: var(--color-spotify);
	}
</style>
