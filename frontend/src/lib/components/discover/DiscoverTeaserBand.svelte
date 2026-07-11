<script lang="ts">
	import { resolve } from '$app/paths';
	import { onMount } from 'svelte';
	import { Compass, Sparkles } from 'lucide-svelte';
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import { discoverQueueStatusStore } from '$lib/stores/discoverQueueStatus';
	import type { DiscoverPreview } from '$lib/types';

	interface Props {
		preview: DiscoverPreview | null;
	}

	let { preview }: Props = $props();

	let queueCount = $state<number | null>(null);

	onMount(async () => {
		// the queue count makes the pitch concrete ("12 albums waiting")
		const status = await discoverQueueStatusStore.fetchStatus();
		if (status?.status === 'ready' && status.item_count) {
			queueCount = status.item_count;
		}
	});

	const teaserArtists = $derived((preview?.items ?? []).slice(0, 4));
	const headline = $derived(
		queueCount
			? `${queueCount} fresh albums are waiting in your Discover queue`
			: preview?.seed_artist
				? `New music picked because you listen to ${preview.seed_artist}`
				: 'Fresh recommendations are waiting for you'
	);
</script>

<a
	href={resolve('/discover')}
	class="group relative block overflow-hidden rounded-2xl border border-primary/15 bg-gradient-to-r from-primary/10 via-base-200/60 to-secondary/10 shadow-[0_4px_24px_oklch(from_var(--color-primary)_l_c_h_/_0.08)] transition-all duration-300 motion-safe:hover:-translate-y-0.5 hover:border-primary/30 hover:shadow-[0_8px_32px_oklch(from_var(--color-primary)_l_c_h_/_0.15)]"
>
	<div class="flex flex-col items-center gap-4 px-5 py-6 sm:flex-row sm:gap-6 sm:px-7">
		{#if teaserArtists.length > 0}
			<div class="flex shrink-0 -space-x-4">
				{#each teaserArtists as artist, i (artist.mbid ?? `${artist.name}-${i}`)}
					<div
						class="h-14 w-14 overflow-hidden rounded-full border-2 border-base-100 shadow-md"
						style="z-index: {teaserArtists.length - i};"
					>
						<ArtistImage
							mbid={artist.mbid ?? ''}
							alt={artist.name}
							size="full"
							requestSize={250}
							lazy={true}
							className="block h-full w-full object-cover"
						/>
					</div>
				{/each}
			</div>
		{:else}
			<div
				class="flex h-14 w-14 shrink-0 items-center justify-center rounded-full bg-primary/15 shadow-[0_0_16px_oklch(from_var(--color-primary)_l_c_h_/_0.2)]"
			>
				<Compass class="h-7 w-7 text-primary" />
			</div>
		{/if}

		<div class="min-w-0 flex-1 text-center sm:text-left">
			<p class="font-mono text-[0.65rem] font-semibold uppercase tracking-widest text-primary/70">
				Picked for you
			</p>
			<h3 class="mt-0.5 text-base font-bold sm:text-lg">{headline}</h3>
			<p class="mt-0.5 text-xs text-base-content/50">
				Sample albums, spin a station, and grab what you love.
			</p>
		</div>

		<span
			class="btn btn-primary btn-sm shrink-0 gap-2 pointer-events-none sm:btn-md"
			aria-hidden="true"
		>
			<Sparkles class="h-4 w-4" />
			Open Discover
		</span>
	</div>
</a>
