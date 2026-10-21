<script lang="ts">
	import { coverSrc } from '$lib/api/covers';
	import { imageSettingsStore } from '$lib/stores/imageSettings';
	import { Check, MicVocal, RefreshCw } from 'lucide-svelte';
	import type { ArtistInfo } from '$lib/types';
	import { extractDominantColor, DEFAULT_GRADIENT } from '$lib/utils/colors';
	import ArtistImage from './ArtistImage.svelte';
	import ArtistLinks from './ArtistLinks.svelte';
	import FollowControl from './FollowControl.svelte';
	import BackButton from './BackButton.svelte';
	import HeroBackdrop from './HeroBackdrop.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import RatingStars from '$lib/ui/RatingStars.svelte';
	import { outsideLink } from './outsideLink';
	import { getArtistImageCreditQuery } from '$lib/queries/artist/ArtistImageCreditQuery.svelte';

	interface Props {
		artist: ArtistInfo;
		showBackButton?: boolean;
		refreshing?: boolean;
		onrefresh?: () => void;
		/** The caller's rating of the library artist, 0 when not rated. */
		rating?: number;
		/** Rate the library artist, or clear it with 0. The stars show only with this. */
		onrate?: (rating: number) => void;
	}

	let {
		artist,
		showBackButton = false,
		refreshing = false,
		onrefresh,
		rating = 0,
		onrate
	}: Props = $props();

	let heroGradient = $state(DEFAULT_GRADIENT);
	let heroImageLoaded = $state(false);

	let resolvedBackdropUrl = $derived.by(() => {
		if ($imageSettingsStore.directRemoteImagesEnabled) {
			if (artist.banner_url) return artist.banner_url;
			if (artist.wide_thumb_url) return artist.wide_thumb_url;
			if (artist.fanart_url) return artist.fanart_url;
		}
		if (heroImageLoaded) return coverSrc('artist', artist.musicbrainz_id, 500);
		return null;
	});

	let hasDistinctBackdrop = $derived(
		$imageSettingsStore.directRemoteImagesEnabled &&
			!!(artist.banner_url || artist.wide_thumb_url || artist.fanart_url)
	);

	function onHeroImageLoad() {
		heroImageLoaded = true;
		extractDominantColor(coverSrc('artist', artist.musicbrainz_id, 500)).then(
			(gradient) => (heroGradient = gradient)
		);
	}

	const creditQuery = getArtistImageCreditQuery(() => ({
		mbid: artist.musicbrainz_id,
		enabled: heroImageLoaded
	}));
	let credit = $derived(creditQuery.data ?? null);
	let creditFileLink = $derived(outsideLink(credit?.file_url));
	let creditLicenceLink = $derived(outsideLink(credit?.licence_url));

	let validLinks = $derived(
		artist.external_links.filter((link) => link.url && link.url.trim() !== '')
	);
</script>

<div
	class="artist-hero group relative -mx-2 sm:-mx-4 lg:-mx-8 -mt-4 sm:-mt-8 overflow-hidden rounded-2xl transition-all duration-500"
>
	<div class="absolute inset-0 bg-gradient-to-b {heroGradient} transition-all duration-1000"></div>

	<HeroBackdrop
		imageUrl={resolvedBackdropUrl}
		opacity={hasDistinctBackdrop ? 0.25 : 0.16}
		hoverOpacity={hasDistinctBackdrop ? 0.35 : 0.22}
		blur={hasDistinctBackdrop ? 0 : 2}
		hoverBlur={0}
		position="full"
	/>

	<div class="relative z-10 px-4 sm:px-8 lg:px-12 pt-6 pb-8 sm:pt-8 sm:pb-12">
		{#if onrefresh && artist.in_library}
			<IconButton
				size="sm"
				icon={RefreshCw}
				label="Refresh artist info"
				class="absolute top-3 right-3 z-20"
				loading={refreshing}
				onclick={onrefresh}
			/>
		{/if}
		<div class="max-w-7xl mx-auto">
			{#if showBackButton}
				<div class="mb-4">
					<BackButton />
				</div>
			{/if}
			<div class="flex flex-col sm:flex-row items-center sm:items-end gap-6 sm:gap-8">
				<div class="shrink-0">
					<div class="relative">
						<div
							class="w-40 h-40 sm:w-52 sm:h-52 lg:w-64 lg:h-64 rounded-full overflow-hidden shadow-2xl ring-4 ring-base-100/20 bg-neutral"
						>
							<ArtistImage
								mbid={artist.musicbrainz_id}
								remoteUrl={artist.thumb_url}
								alt={artist.name}
								size="hero"
								rounded="none"
								priority
								onload={onHeroImageLoad}
							/>
						</div>
						{#if artist.in_library}
							<div class="absolute -bottom-2 -right-2 badge badge-success badge-lg gap-1 shadow-lg">
								<Check class="h-4 w-4" />
								In Library
							</div>
						{:else if artist.appears_in_library}
							<div class="absolute -bottom-2 -right-2 badge badge-accent badge-lg gap-1 shadow-lg">
								<MicVocal class="h-4 w-4" />
								Appears in library
							</div>
						{/if}
					</div>
				</div>

				<div class="flex-1 text-center sm:text-left min-w-0">
					{#if artist.type}
						<span
							class="text-xs sm:text-sm font-medium text-base-content/70 uppercase tracking-wider"
						>
							{artist.type === 'Group' ? 'Band' : artist.type === 'Person' ? 'Artist' : artist.type}
						</span>
					{/if}
					<h1
						class="hero-title text-3xl sm:text-4xl lg:text-5xl xl:text-6xl font-bold text-base-content mt-1 mb-2 break-words"
					>
						{artist.name}
					</h1>
					{#if artist.disambiguation}
						<p class="text-base-content/60 text-sm sm:text-base mb-3">({artist.disambiguation})</p>
					{/if}

					{#if validLinks.length > 0}
						<ArtistLinks links={validLinks} />
					{/if}

					{#if onrate}
						<div class="mt-2">
							<RatingStars size="md" value={rating} label={artist.name} onchange={onrate} />
						</div>
					{/if}

					<div class="mt-3">
						<FollowControl artistMbid={artist.musicbrainz_id} />
					</div>
				</div>
			</div>
		</div>
	</div>

	{#if credit && creditFileLink}
		<p
			class="absolute bottom-2 right-3 left-3 z-10 truncate text-right text-[0.7rem] text-base-content/50"
		>
			<a
				href={creditFileLink}
				target="_blank"
				rel="noopener noreferrer"
				class="hover:text-base-content/80 hover:underline"
				>Photo: {credit.author ?? 'Wikimedia Commons'}</a
			>{#if credit.licence},
				{#if creditLicenceLink}
					<a
						href={creditLicenceLink}
						target="_blank"
						rel="noopener noreferrer license"
						class="hover:text-base-content/80 hover:underline">{credit.licence}</a
					>
				{:else}
					{credit.licence}
				{/if}
			{/if}
		</p>
	{/if}
</div>

<style>
	.artist-hero {
		--hero-glow-color: var(--brand-hero);
		border: 1px solid rgb(var(--brand-hero) / 0.08);
		animation: hero-glow 4s ease-in-out infinite;
	}
	.artist-hero:hover {
		border-color: rgb(var(--brand-hero) / 0.2);
	}
	@media (prefers-reduced-motion: reduce) {
		.artist-hero {
			animation: none;
		}
	}
</style>
