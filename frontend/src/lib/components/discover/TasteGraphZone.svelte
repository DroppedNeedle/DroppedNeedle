<script lang="ts">
	/*
	 * TasteGraphZone — recommendations grown from the user's own collection via
	 * canonical MusicBrainz relationships (collaborators, members, labels,
	 * scenes). No charts, no crowds. Old backends lack the route, so any fetch
	 * error settles into the same quiet invitation panel as a cold start.
	 */
	import { Network, Check } from 'lucide-svelte';
	import { integrationStore } from '$lib/stores/integration';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import AlbumRequestButton from '$lib/components/AlbumRequestButton.svelte';
	import RadioPlayButton from '$lib/components/discover/RadioPlayButton.svelte';
	import { getTasteGraphQuery } from '$lib/queries/discover/TasteGraphQuery.svelte';

	const tasteGraphQuery = getTasteGraphQuery();
	const data = $derived(tasteGraphQuery.data ?? null);
	const loading = $derived(tasteGraphQuery.isLoading);
	// error (incl. 404 from old backends) and cold start share the invitation panel
	const showInvitation = $derived(
		!!tasteGraphQuery.error || (!!data && (data.cold_start || data.items.length === 0))
	);
</script>

<section aria-label="Your taste graph">
	<h3
		class="mb-1 flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
	>
		<Network class="h-4 w-4 text-accent" />
		Your Taste Graph
	</h3>
	<p class="mb-5 text-xs text-base-content/50">
		Grown from your collection and canonical MusicBrainz relationships — no charts, no crowds.
	</p>

	{#if loading}
		<div class="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-5">
			{#each Array(10) as _, i (`taste-graph-skeleton-${i}`)}
				<div class="rounded-2xl border border-base-content/8 bg-base-200/50 p-3">
					<div class="skeleton skeleton-shimmer aspect-square w-full rounded-xl"></div>
					<div class="skeleton skeleton-shimmer mt-3 h-4 w-3/4"></div>
					<div class="skeleton skeleton-shimmer mt-2 h-3 w-1/2"></div>
				</div>
			{/each}
		</div>
	{:else if showInvitation}
		<div
			class="flex flex-col items-center rounded-2xl border border-dashed border-base-content/12 px-6 py-10 text-center"
		>
			<Network class="mb-3 h-8 w-8 text-base-content/40" />
			<p class="max-w-md text-sm text-base-content/60">
				The graph grows from your music — scan your library or follow artists and check back.
			</p>
		</div>
	{:else if data}
		{#if data.seeds.length > 0}
			<div class="mb-5 flex flex-wrap items-center gap-2">
				<span
					class="font-mono text-[0.62rem] font-bold uppercase tracking-[0.2em] text-base-content/40"
					>Seeded by:</span
				>
				{#each data.seeds as seed (seed.artist_mbid)}
					<span
						class="rounded-full border border-base-content/10 px-2.5 py-0.5 text-xs text-base-content/60"
						>{seed.name}</span
					>
				{/each}
			</div>
		{/if}

		<div class="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-5">
			{#each data.items as item (`${item.kind}-${item.mbid}`)}
				<div
					class="group flex flex-col rounded-2xl border border-base-content/8 bg-base-200/50 p-3 transition-colors hover:border-primary/30"
				>
					<a
						href={item.kind === 'album' ? `/album/${item.mbid}` : `/artist/${item.mbid}`}
						data-sveltekit-preload-data="hover"
						class="min-w-0"
					>
						<div class="aspect-square w-full overflow-hidden rounded-xl">
							{#if item.kind === 'album'}
								<AlbumImage mbid={item.mbid} alt={item.name} size="md" className="h-full w-full" />
							{:else}
								<ArtistImage
									mbid={item.mbid}
									alt={item.name}
									size="md"
									rounded="lg"
									className="h-full w-full"
								/>
							{/if}
						</div>
						<h4 class="mt-3 truncate font-display text-sm font-semibold tracking-tight">
							{item.name}
						</h4>
						{#if item.kind === 'album' && item.artist_name}
							<p class="mt-0.5 truncate text-xs text-base-content/50">{item.artist_name}</p>
						{/if}
					</a>

					{#if item.reasons.length > 0 || item.in_library}
						<div class="mt-2 flex flex-wrap gap-1">
							{#if item.in_library}
								<span
									class="inline-flex items-center gap-1 rounded-full bg-accent/15 px-2 py-0.5 font-mono text-[0.55rem] font-bold uppercase tracking-[0.15em] text-accent"
								>
									<Check class="h-2.5 w-2.5" />
									In collection
								</span>
							{/if}
							{#each item.reasons as reason, i (`${item.mbid}-reason-${i}`)}
								<span
									class="rounded-full border border-base-content/10 px-2 py-0.5 font-mono text-[0.55rem] font-bold uppercase tracking-[0.15em] text-base-content/50"
									>{reason.label}</span
								>
							{/each}
						</div>
					{/if}

					<div class="mt-auto flex items-center justify-end gap-2 pt-2">
						{#if item.kind === 'artist'}
							<RadioPlayButton
								seed={{ seed_type: 'artist', seed_id: item.mbid }}
								size="sm"
								label=""
							/>
						{:else if !item.in_library && $integrationStore.download_client}
							<AlbumRequestButton
								mbid={item.mbid}
								artistName={item.artist_name ?? ''}
								albumName={item.name}
								artistMbid={item.artist_mbid ?? undefined}
							/>
						{/if}
					</div>
				</div>
			{/each}
		</div>
	{/if}
</section>
