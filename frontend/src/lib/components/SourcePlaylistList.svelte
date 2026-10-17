<script lang="ts">
	import type { Snippet } from 'svelte';
	import { ApiError } from '$lib/api/client';
	import BackButton from '$lib/components/BackButton.svelte';
	import SourcePlaylistCard from '$lib/components/SourcePlaylistCard.svelte';
	import { getSourcePlaylistsQuery } from '$lib/queries/source-playlists/SourcePlaylistQueries.svelte';
	import type { SourcePlaylistSource } from '$lib/types';
	import { withBasePath } from '$lib/utils/basePath';
	import { CircleAlert, Link2, ListMusic, RefreshCw } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		source: SourcePlaylistSource;
		sourceLabel: string;
		backFallback: string;
		playlistBaseHref: string;
		icon: Snippet;
	}

	const { source, sourceLabel, backFallback, playlistBaseHref, icon }: Props = $props();
	const playlistsQuery = getSourcePlaylistsQuery(() => source);
	const collection = $derived(playlistsQuery.data);
	const playlists = $derived(collection?.playlists ?? []);
	const relinkRequired = $derived(
		playlistsQuery.error instanceof ApiError &&
			playlistsQuery.error.code === 'MEDIA_ACCOUNT_RELINK_REQUIRED'
	);
</script>

<div class="mx-auto max-w-6xl space-y-6 px-4 py-6">
	<div class="flex flex-wrap items-center gap-3">
		<BackButton fallback={backFallback} />
		{@render icon()}
		<div class="min-w-0">
			<h1 class="text-2xl font-bold">{sourceLabel} Playlists</h1>
			{#if collection}
				<p class="text-xs text-base-content/50">
					{collection.account_mode === 'linked'
						? `Available to ${collection.account_label}`
						: `Using the shared ${sourceLabel} account`}
				</p>
			{/if}
		</div>
	</div>

	{#if playlistsQuery.isPending}
		<div class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6">
			{#each Array(12) as _, index (index)}
				<Card variant="bordered" padding="sm">
					<div class="skeleton aspect-square w-full rounded-lg"></div>
					<div class="skeleton h-4 w-4/5"></div>
					<div class="skeleton h-3 w-3/5"></div>
				</Card>
			{/each}
		</div>
	{:else if playlistsQuery.isError}
		<div class="rounded-2xl border border-warning/20 bg-warning/5 p-6 text-center">
			<CircleAlert class="mx-auto h-9 w-9 text-warning" />
			<h2 class="mt-3 text-lg font-semibold">
				{relinkRequired ? `Reconnect ${sourceLabel}` : `Couldn't load ${sourceLabel} playlists`}
			</h2>
			<p class="mx-auto mt-1 max-w-lg text-sm text-base-content/55">
				{relinkRequired
					? `Reconnect your ${sourceLabel} account so DroppedNeedle can load its playlists.`
					: "We couldn't load playlists from this server. Try again in a moment."}
			</p>
			<div class="mt-4 flex flex-wrap justify-center gap-2">
				{#if relinkRequired}
					<Button
						variant="primary"
						size="sm"
						icon={Link2}
						href={withBasePath('/profile#media-accounts')}
					>
						Reconnect account
					</Button>
				{/if}
				<Button
					variant="ghost"
					size="sm"
					icon={RefreshCw}
					onclick={() => void playlistsQuery.refetch()}
				>
					Try again
				</Button>
			</div>
		</div>
	{:else if playlists.length === 0}
		<Card variant="bordered" class="py-6 text-center">
			<ListMusic class="mx-auto h-10 w-10 text-base-content/25" />
			<h2 class="mt-3 text-lg font-semibold">No playlists found</h2>
			<p class="mx-auto mt-1 max-w-lg text-sm text-base-content/50">
				{collection?.account_mode === 'linked'
					? `${collection.account_label} has no playlists available to import.`
					: `The shared ${sourceLabel} account has no playlists. Link your account to see yours.`}
			</p>
			<div class="mt-4 flex flex-wrap justify-center gap-2">
				{#if collection}
					<Button
						variant="primary"
						size="sm"
						icon={Link2}
						href={withBasePath('/profile#media-accounts')}
					>
						{collection.account_mode === 'shared' ? 'Link your account' : 'Manage account'}
					</Button>
				{/if}
				<Button
					variant="ghost"
					size="sm"
					icon={RefreshCw}
					onclick={() => void playlistsQuery.refetch()}
				>
					Check again
				</Button>
			</div>
		</Card>
	{:else}
		<div class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6">
			{#each playlists as playlist (playlist.id)}
				<SourcePlaylistCard {playlist} href={withBasePath(`${playlistBaseHref}/${playlist.id}`)} />
			{/each}
		</div>
	{/if}
</div>
