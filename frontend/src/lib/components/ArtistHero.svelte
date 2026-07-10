<script lang="ts">
	import { Check, RefreshCw } from 'lucide-svelte';
	import type { ArtistInfo } from '$lib/types';
	import { extractDominantColor, DEFAULT_GRADIENT } from '$lib/utils/colors';
	import { imageSettingsStore } from '$lib/stores/imageSettings';
	import ArtistImage from './ArtistImage.svelte';
	import ArtistLinks from './ArtistLinks.svelte';
	import FollowControl from './FollowControl.svelte';
	import BackButton from './BackButton.svelte';
	import { getApiUrl } from '$lib/api/api-utils';

	interface Props {
		artist: ArtistInfo;
		showBackButton?: boolean;
		refreshing?: boolean;
		onrefresh?: () => void;
	}

	let { artist, showBackButton = false, refreshing = false, onrefresh }: Props = $props();

	let heroGradient = $state(DEFAULT_GRADIENT);
	let heroImageLoaded = $state(false);

	let resolvedBackdropUrl = $derived.by(() => {
		if ($imageSettingsStore.directRemoteImagesEnabled) {
			if (artist.banner_url) return artist.banner_url;
			if (artist.wide_thumb_url) return artist.wide_thumb_url;
			if (artist.fanart_url) return artist.fanart_url;
		}
		if (heroImageLoaded)
			return getApiUrl(`/api/v1/covers/artist/${artist.musicbrainz_id}?size=500`);
		return null;
	});

	let hasDistinctBackdrop = $derived(
		$imageSettingsStore.directRemoteImagesEnabled &&
			!!(artist.banner_url || artist.wide_thumb_url || artist.fanart_url)
	);

	function onHeroImageLoad() {
		heroImageLoaded = true;
		extractDominantColor(getApiUrl(`/api/v1/covers/artist/${artist.musicbrainz_id}?size=250`)).then(
			(gradient) => (heroGradient = gradient)
		);
	}

	let validLinks = $derived(
		artist.external_links.filter((link) => link.url && link.url.trim() !== '')
	);
</script>

<div
	class="artist-hero group relative isolate -mx-2 -mt-4 overflow-hidden sm:-mx-4 sm:-mt-8 lg:-mx-8"
>
	<div class="absolute inset-0 -z-10" aria-hidden="true">
		<div
			class="absolute inset-0 bg-gradient-to-b {heroGradient} opacity-50 transition-all duration-1000"
		></div>
		{#if resolvedBackdropUrl}
			{#key resolvedBackdropUrl}
				<img
					src={resolvedBackdropUrl}
					alt=""
					class="artist-hero__img"
					class:artist-hero__img--soft={!hasDistinctBackdrop}
				/>
			{/key}
		{/if}
		<div class="artist-hero__wash"></div>
	</div>

	<div class="relative px-4 pt-6 pb-10 sm:px-8 sm:pt-8 sm:pb-14 lg:px-12">
		{#if onrefresh && artist.in_library}
			<button
				class="btn btn-sm btn-ghost btn-circle absolute top-4 right-4 z-20"
				onclick={onrefresh}
				disabled={refreshing}
				title="Refresh artist info"
			>
				<RefreshCw class="h-5 w-5 {refreshing ? 'animate-spin' : ''}" />
			</button>
		{/if}

		{#if showBackButton}
			<div class="mb-6">
				<BackButton />
			</div>
		{/if}

		<div class="flex flex-col items-center gap-6 sm:flex-row sm:items-end sm:gap-8">
			<div class="shrink-0">
				<div class="relative">
					<div
						class="h-40 w-40 overflow-hidden rounded-full bg-neutral shadow-2xl ring-1 ring-base-content/15 sm:h-52 sm:w-52 lg:h-60 lg:w-60"
					>
						<ArtistImage
							mbid={artist.musicbrainz_id}
							remoteUrl={artist.thumb_url}
							alt={artist.name}
							size="hero"
							rounded="none"
							onload={onHeroImageLoad}
						/>
					</div>
					{#if artist.in_library}
						<span
							class="absolute -right-2 -bottom-1 inline-flex items-center gap-1.5 rounded-full border border-accent/40 bg-base-100/80 px-3 py-1 font-mono text-[0.62rem] font-bold uppercase tracking-[0.16em] text-accent shadow-lg backdrop-blur"
						>
							<Check class="h-3.5 w-3.5" />
							In Library
						</span>
					{/if}
				</div>
			</div>

			<div class="min-w-0 flex-1 text-center sm:text-left">
				<p class="artist-hero__eyebrow">
					<span>
						{artist.type === 'Group'
							? 'Band'
							: artist.type === 'Person'
								? 'Artist'
								: artist.type || 'Artist'}
					</span>
				</p>

				<h1
					class="hero-title mt-3 mb-2 break-words font-display text-4xl font-bold leading-[1.02] tracking-tight text-base-content sm:text-5xl lg:text-6xl"
				>
					{artist.name}
				</h1>

				{#if artist.disambiguation}
					<p class="mb-3 text-sm text-base-content/60 sm:text-base">({artist.disambiguation})</p>
				{/if}

				{#if validLinks.length > 0}
					<ArtistLinks links={validLinks} />
				{/if}

				<div class="mt-4">
					<FollowControl artistMbid={artist.musicbrainz_id} />
				</div>
			</div>
		</div>
	</div>
</div>

<style>
	.artist-hero__img {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		object-fit: cover;
		object-position: center 25%;
		filter: saturate(0.9) brightness(0.72);
		transition: opacity var(--dn-dur-slow) ease;
	}
	.artist-hero__img--soft {
		filter: saturate(0.85) brightness(0.65) blur(8px);
		transform: scale(1.05);
	}

	/* charcoal wash — same technique as .dn-spotlight__wash on Home */
	.artist-hero__wash {
		position: absolute;
		inset: 0;
		background:
			linear-gradient(
				90deg,
				oklch(from var(--color-base-100) l c h / 0.92) 0%,
				oklch(from var(--color-base-100) l c h / 0.55) 45%,
				oklch(from var(--color-base-100) l c h / 0.2) 100%
			),
			linear-gradient(
				180deg,
				oklch(from var(--color-base-100) l c h / 0.35) 0%,
				oklch(from var(--color-base-100) l c h / 0.1) 40%,
				var(--color-base-100) 100%
			);
	}

	.artist-hero__eyebrow span {
		display: inline-block;
		padding: 0.32rem 0.85rem;
		border-radius: 999px;
		border: 1px solid var(--dn-hairline-strong);
		background: oklch(from var(--color-base-100) l c h / 0.45);
		backdrop-filter: blur(8px);
		font-family: var(--font-mono);
		font-size: 0.62rem;
		font-weight: 700;
		letter-spacing: 0.22em;
		text-transform: uppercase;
		color: oklch(from var(--color-base-content) l c h / 0.75);
	}
</style>
