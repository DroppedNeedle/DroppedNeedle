<script lang="ts">
	import { useDirectRemoteImages } from '$lib/features/settings/queries/AdvancedSettingsReads.svelte';
	import type { BecauseYouListenTo } from '$lib/features/discover/queries/types';
	import HomeSection from './HomeSection.svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import { Headphones } from 'lucide-svelte';

	const directRemoteImages = useDirectRemoteImages();

	interface Props {
		entry: BecauseYouListenTo;
	}

	let { entry }: Props = $props();

	let avatarRemoteUrl = $derived.by(() => {
		if (directRemoteImages.current) {
			if (entry.banner_url) return entry.banner_url;
			if (entry.wide_thumb_url) return entry.wide_thumb_url;
			if (entry.fanart_url) return entry.fanart_url;
		}
		return null;
	});
</script>

<article class="relative overflow-hidden rounded-xl border border-line bg-base-200">
	<div class="relative flex items-center gap-3 px-4 pt-4 pb-2 sm:px-5 sm:pt-5">
		<div
			class="relative h-11 w-11 sm:h-12 sm:w-12 shrink-0 overflow-hidden rounded-full ring-2 ring-action"
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
				class="mb-1 font-mono text-[10px] font-semibold leading-none tracking-[0.2em] text-muted uppercase sm:text-xs"
			>
				More like
			</div>
			<h3 class="truncate font-display text-xl leading-none text-title uppercase sm:text-2xl">
				{entry.seed_artist}
			</h3>
		</div>
		{#if entry.listen_count}
			<span
				class="hidden shrink-0 items-center gap-1.5 rounded-full border border-edge px-2.5 py-1 font-mono text-xs font-semibold sm:inline-flex"
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
