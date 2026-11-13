<script lang="ts">
	import { Check } from 'lucide-svelte';
	import type { ArtistRef } from '$lib/features/discover/queries/types';
	import HorizontalCarousel from '$lib/ui/HorizontalCarousel.svelte';
	import CarouselSkeleton from '$lib/ui/CarouselSkeleton.svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import { artistHref } from '$lib/utils/entityRoutes';
	import type { components } from '$lib/api/v3/openapi';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import Button from '$lib/ui/Button.svelte';

	interface Props {
		artists: ArtistRef[];
		loading?: boolean;
		configured?: boolean;
		/** The service that answered; unconfigured ListenBrainz means an admin turned it off. */
		source?: components['schemas']['ListenSource'];
	}

	let { artists, loading = false, configured = true, source }: Props = $props();

	const linked = $derived(
		artists.flatMap((artist) =>
			artist.musicbrainz_id ? [{ ...artist, musicbrainz_id: artist.musicbrainz_id }] : []
		)
	);
</script>

{#if loading}
	<CarouselSkeleton count={8} cardWidth="w-32" rounded="full" showSubtitle={false} />
{:else if !configured && source === 'listenbrainz'}
	<div class="flex flex-wrap items-center gap-3 text-sm text-muted">
		<p>Recommendations are turned off.</p>
		{#if authStore.isAdmin}
			<Button variant="primary" size="sm" href={withBasePath('/settings?tab=listenbrainz')}
				>Settings</Button
			>
		{/if}
	</div>
{:else if !configured}
	<div class="flex flex-wrap items-center gap-3 text-sm text-muted">
		<p>Connect a music service to see similar artists</p>
		<Button variant="primary" size="sm" href={withBasePath('/profile#scrobbling')}>Configure</Button
		>
	</div>
{:else if artists.length === 0}
	<p class="text-sm text-muted">No similar artists found</p>
{:else}
	<HorizontalCarousel>
		{#each linked as artist (artist.musicbrainz_id)}
			<MediaTile
				kind="artist"
				title={artist.name}
				href={artistHref(artist.musicbrainz_id)}
				class="w-32 shrink-0 sm:w-36"
			>
				{#snippet art()}
					<ArtistImage
						mbid={artist.musicbrainz_id}
						alt={artist.name}
						size="full"
						requestSize={250}
						className="h-full w-full"
					/>
				{/snippet}
				{#snippet overlay()}
					{#if artist.in_library}
						<StatusChip tone="action" size="xs" icon={Check} class="absolute right-3 bottom-1">
							<span class="sr-only">In library</span>
						</StatusChip>
					{/if}
				{/snippet}
			</MediaTile>
		{/each}
	</HorizontalCarousel>
{/if}
