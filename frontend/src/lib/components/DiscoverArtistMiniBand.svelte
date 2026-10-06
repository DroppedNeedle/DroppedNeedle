<script lang="ts">
	import { coverSrc } from '$lib/api/covers';
	import { imageSettingsStore } from '$lib/stores/imageSettings';
	import type { BecauseYouListenTo } from '$lib/types';
	import HomeSection from './HomeSection.svelte';
	import HeroBackdrop from './HeroBackdrop.svelte';
	import ArtistImage from './ArtistImage.svelte';
	import { Headphones } from 'lucide-svelte';

	interface Props {
		entry: BecauseYouListenTo;
	}

	let { entry }: Props = $props();

	let hasDirectBackdrop = $derived(
		$imageSettingsStore.directRemoteImagesEnabled &&
			!!(entry.banner_url || entry.wide_thumb_url || entry.fanart_url)
	);

	let backdropUrl = $derived.by(() => {
		if ($imageSettingsStore.directRemoteImagesEnabled) {
			if (entry.banner_url) return entry.banner_url;
			if (entry.wide_thumb_url) return entry.wide_thumb_url;
			if (entry.fanart_url) return entry.fanart_url;
		}
		return entry.seed_artist_mbid ? coverSrc('artist', entry.seed_artist_mbid, 500) : null;
	});

	let avatarRemoteUrl = $derived.by(() => {
		if ($imageSettingsStore.directRemoteImagesEnabled) {
			if (entry.banner_url) return entry.banner_url;
			if (entry.wide_thumb_url) return entry.wide_thumb_url;
			if (entry.fanart_url) return entry.fanart_url;
		}
		return null;
	});
</script>

<article
	class="mini-band group relative overflow-hidden rounded-2xl transition-all duration-500 hover:shadow-[0_0_28px_rgb(var(--brand-discover)/0.1)]"
>
	<HeroBackdrop
		imageUrl={backdropUrl}
		opacity={hasDirectBackdrop ? 0.14 : 0.08}
		hoverOpacity={hasDirectBackdrop ? 0.2 : 0.12}
		blur={hasDirectBackdrop ? 0 : 2}
		hoverBlur={hasDirectBackdrop ? 0 : 1}
		position="full"
	/>

	<div class="relative flex items-center gap-3 px-4 pt-4 pb-2 sm:px-5 sm:pt-5">
		<div
			class="relative h-11 w-11 sm:h-12 sm:w-12 shrink-0 overflow-hidden rounded-full ring-2 ring-[rgb(var(--brand-discover)/0.35)] shadow-[0_0_18px_rgb(var(--brand-discover)/0.25)]"
		>
			<ArtistImage
				mbid={entry.seed_artist_mbid}
				alt={entry.seed_artist}
				size="sm"
				rounded="full"
				remoteUrl={avatarRemoteUrl}
			/>
		</div>
		<div class="flex-1 min-w-0">
			<div
				class="text-[10px] sm:text-xs font-semibold uppercase tracking-widest text-base-content/40 leading-none mb-1"
			>
				More like
			</div>
			<h3 class="text-base sm:text-lg font-bold text-primary truncate leading-tight">
				{entry.seed_artist}
			</h3>
		</div>
		{#if entry.listen_count > 0}
			<span
				class="hidden sm:inline-flex items-center gap-1.5 shrink-0 rounded-full px-2.5 py-1 text-xs font-medium"
				style="background: rgb(var(--brand-discover) / 0.12); color: rgb(var(--brand-discover));"
			>
				<Headphones class="w-3 h-3" />
				{entry.listen_count}
			</span>
		{/if}
	</div>

	<div class="relative px-4 sm:px-5 pb-1">
		<HomeSection section={entry.section} hideHeader />
	</div>
</article>

<style>
	.mini-band {
		background: linear-gradient(
			135deg,
			rgb(var(--brand-discover) / 0.05) 0%,
			rgb(var(--brand-discover) / 0.015) 50%,
			transparent 100%
		);
		border: 1px solid rgb(var(--brand-discover) / 0.08);
	}
	.mini-band:hover {
		border-color: rgb(var(--brand-discover) / 0.18);
	}
</style>
