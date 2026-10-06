<script lang="ts">
	import { coverSrc } from '$lib/api/covers';
	import { imageSettingsStore } from '$lib/stores/imageSettings';
	import type { BecauseYouListenTo } from '$lib/types';
	import HomeSection from './HomeSection.svelte';
	import HeroBackdrop from './HeroBackdrop.svelte';
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
</script>

<article
	class="discover-hero group relative overflow-hidden rounded-2xl transition-all duration-500 hover:shadow-[0_0_40px_rgb(var(--brand-discover)/0.12)]"
>
	<HeroBackdrop
		imageUrl={backdropUrl}
		opacity={hasDirectBackdrop ? 0.2 : 0.12}
		hoverOpacity={hasDirectBackdrop ? 0.3 : 0.18}
		blur={hasDirectBackdrop ? 0 : 2}
		hoverBlur={hasDirectBackdrop ? 0 : 1}
		position="full"
	/>

	<div class="relative px-4 pt-5 pb-1 sm:px-6 sm:pt-6">
		<div class="flex items-center gap-3 mb-1">
			<div class="flex items-center gap-2 min-w-0">
				<span class="text-xs font-semibold uppercase tracking-widest text-base-content/40"
					>Because You Listen To</span
				>
			</div>
		</div>

		<div class="flex items-center gap-3 mb-1">
			<h2 class="text-xl sm:text-2xl font-bold text-primary truncate">
				{entry.seed_artist}
			</h2>
			{#if entry.listen_count > 0}
				<span
					class="inline-flex items-center gap-1.5 shrink-0 rounded-full px-3 py-1 text-xs font-medium"
					style="background: rgb(var(--brand-discover) / 0.12); color: rgb(var(--brand-discover));"
				>
					<Headphones class="w-3 h-3" />
					{entry.listen_count} listens this week
				</span>
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
	.discover-hero {
		--hero-glow-color: var(--brand-discover);
		background: linear-gradient(
			135deg,
			rgb(var(--brand-discover) / 0.06) 0%,
			rgb(var(--brand-discover) / 0.02) 40%,
			transparent 100%
		);
		border: 1px solid rgb(var(--brand-discover) / 0.08);
		animation: hero-glow 4s ease-in-out infinite;
	}
	.discover-hero:hover {
		border-color: rgb(var(--brand-discover) / 0.2);
	}

	@media (prefers-reduced-motion: reduce) {
		.discover-hero {
			animation: none;
		}
	}
</style>
