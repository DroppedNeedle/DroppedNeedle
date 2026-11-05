<script lang="ts">
	import { useDirectRemoteImages } from '$lib/features/settings/queries/AdvancedSettingsReads.svelte';
	import type { BecauseYouListenTo } from '$lib/features/discover/queries/types';
	import HomeSection from './HomeSection.svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import HalftonePhoto from '$lib/ui/HalftonePhoto.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import { Headphones } from 'lucide-svelte';

	const directRemoteImages = useDirectRemoteImages();

	interface Props {
		entry: BecauseYouListenTo;
	}

	let { entry }: Props = $props();

	// a wide picture the provider gave, when remote images may load directly;
	// otherwise the artist's own picture through the server
	let photoUrl = $derived(
		directRemoteImages.current
			? (entry.banner_url ?? entry.wide_thumb_url ?? entry.fanart_url ?? null)
			: null
	);
	let photoLoaded = $state(false);
</script>

<article class="dn-because relative overflow-hidden rounded-xl border border-line bg-base-200">
	{#if photoUrl || entry.seed_artist_mbid}
		<HalftonePhoto
			class="dn-because__photo"
			loaded={photoLoaded}
			fallback={entry.seed_artist.slice(0, 2)}
		>
			<ArtistImage
				mbid={entry.seed_artist_mbid}
				alt=""
				size="full"
				requestSize={1200}
				rounded="none"
				showPlaceholder={false}
				remoteUrl={photoUrl}
				className="h-full w-full bg-transparent!"
				onload={() => (photoLoaded = true)}
			/>
		</HalftonePhoto>
	{/if}

	<div class="relative px-4 pt-5 pb-1 sm:px-6 sm:pt-6">
		<p class="font-mono text-xs font-semibold tracking-[0.2em] text-muted uppercase">
			Because You Listen To
		</p>
		<div class="mt-1.5 mb-3 flex flex-wrap items-center gap-3">
			<h2 class="font-display text-4xl leading-none text-title uppercase sm:text-5xl">
				{entry.seed_artist}
			</h2>
			{#if entry.listen_count}
				<StatusChip tone="action" size="sm" icon={Headphones}>
					{entry.listen_count} listens this week
				</StatusChip>
			{/if}
		</div>
	</div>

	<div class="relative px-4 sm:px-6">
		<HomeSection
			section={{
				...entry.section,
				title: `Similar to ${entry.seed_artist}`
			}}
		/>
	</div>
</article>

<style>
	/* the artist printed as a halftone at the band's right, fading left */
	.dn-because :global(.dn-because__photo) {
		position: absolute;
		inset: 0 0 auto auto;
		width: min(60%, 36rem);
		height: 15rem;
	}
	@media (max-width: 639px) {
		.dn-because :global(.dn-because__photo) {
			width: 100%;
			height: 10rem;
		}
	}
</style>
