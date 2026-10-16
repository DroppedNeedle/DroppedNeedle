<script lang="ts">
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import SpotifyIcon from '$lib/components/SpotifyIcon.svelte';
	import { getConnectionsQuery } from '$lib/queries/connections/ConnectionsQuery.svelte';
	import {
		createConnectSpotifyMutation,
		createDisconnectMutation
	} from '$lib/queries/connections/ConnectionsMutations.svelte';
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

<section>
	<h2
		class="mb-4 flex items-center gap-2 text-sm font-semibold uppercase tracking-widest text-base-content/50"
	>
		<SpotifyIcon class="h-4 w-4 text-spotify" />
		Spotify
	</h2>

	<Card variant="bordered" padding="md">
		<div class="space-y-3">
			<Card variant="bordered" padding="sm">
				<div class="flex items-center justify-between gap-3">
					<div class="flex min-w-0 items-center gap-3">
						<div
							class="flex h-10 w-10 items-center justify-center rounded-xl bg-spotify/10 text-spotify ring-1 ring-spotify/20"
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
							<Button
								variant="ghost"
								size="xs"
								pill
								loading={disconnectMutation.isPending}
								onclick={disconnect}
							>
								Disconnect
							</Button>
						{:else}
							<Button
								variant="primary"
								size="xs"
								pill
								loading={connectMutation.isPending}
								onclick={connect}
							>
								{#if !connectMutation.isPending}
									<SpotifyIcon class="h-3.5 w-3.5" />
								{/if}
								Connect
							</Button>
						{/if}
					</div>
				</div>
			</Card>

			{#if spotify}
				<p class="px-1 text-xs text-base-content/50">
					Connected. Go to your <a href={withBasePath('/playlists')} class="link link-primary"
						>Playlists</a
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
	</Card>
</section>
