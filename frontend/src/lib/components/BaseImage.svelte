<script lang="ts">
	import { coverSrc, type CoverKind } from '$lib/api/covers';
	import { imageSettingsStore } from '$lib/stores/imageSettings';
	import { onDestroy } from 'svelte';
	import { Disc3, Users } from 'lucide-svelte';
	import { lazyImage, resetLazyImage } from '$lib/utils/lazyImage';
	import { API_SIZES } from '$lib/constants';
	import { isValidMbid } from '$lib/utils/formatting';
	import { appendAudioDBSizeSuffix } from '$lib/utils/imageSuffix';
	import { getApiUrl } from '$lib/api/api-utils';
	import {
		COVER_VISUAL_SETTLE_MS,
		watchWarmingCover,
		type CoverWarmUpdate
	} from '$lib/utils/coverWarmCoordinator';

	interface Props {
		mbid: string;
		alt?: string;
		size?: 'xs' | 'sm' | 'md' | 'lg' | 'xl' | 'hero' | 'full';
		requestSize?: 250 | 500 | 1200;
		responsiveSizes?: string;
		lazy?: boolean;
		showPlaceholder?: boolean;
		className?: string;
		rounded?: 'none' | 'sm' | 'md' | 'lg' | 'xl' | 'full';
		customUrl?: string | null;
		remoteUrl?: string | null;
		imageType?: 'album' | 'artist';
		source?: 'provider' | 'local';
		available?: boolean;
		retryOnError?: boolean;
		transparentFallback?: boolean;
		testId?: string;
		onload?: () => void;
	}

	let {
		mbid,
		alt = 'Image',
		size = 'md',
		requestSize = undefined,
		responsiveSizes = undefined,
		lazy = true,
		showPlaceholder = true,
		className = '',
		rounded = 'lg',
		customUrl = null,
		remoteUrl = null,
		imageType = 'album',
		source = 'provider',
		available = true,
		retryOnError = true,
		transparentFallback = false,
		testId = undefined,
		onload = undefined
	}: Props = $props();

	let imgError = $state(false);
	let failed = $state(false);
	let imgLoaded = $state(false);
	let visualSettled = $state(false);
	let remoteError = $state(false);
	let imgElement: HTMLImageElement | null = $state(null);
	let currentSource = $state('');
	let warmResolvedUrl: string | null = $state(null);
	let stopWatchingWarmCover: (() => void) | null = null;
	let visualSettleTimer: ReturnType<typeof setTimeout> | null = null;
	let retrySourceKey = $state('');

	const albumSizeClasses: Record<typeof size, string> = {
		xs: 'w-8 h-8',
		sm: 'w-12 h-12',
		md: 'w-16 h-16',
		lg: 'w-24 h-24 sm:w-32 sm:h-32',
		xl: 'w-36 h-36 sm:w-44 sm:h-44',
		hero: 'w-48 h-48 sm:w-64 sm:h-64 lg:w-80 lg:h-80',
		full: ''
	};

	const artistSizeClasses: Record<typeof size, string> = {
		xs: 'w-8 h-8',
		sm: 'w-12 h-12',
		md: 'w-28 h-28 sm:w-36 sm:h-36',
		lg: 'w-36 h-36 sm:w-44 sm:h-44',
		xl: 'w-48 h-48 sm:w-56 sm:h-56',
		hero: 'w-40 h-40 sm:w-52 sm:h-52 lg:w-64 lg:h-64',
		full: ''
	};

	const roundedClasses: Record<typeof rounded, string> = {
		none: '',
		sm: 'rounded-sm',
		md: 'rounded-md',
		lg: 'rounded-lg',
		xl: 'rounded-xl',
		full: 'rounded-full'
	};

	const apiSizes: Record<typeof size, number> = {
		xs: API_SIZES.XS,
		sm: API_SIZES.SM,
		md: API_SIZES.MD,
		lg: API_SIZES.LG,
		xl: API_SIZES.XL,
		hero: API_SIZES.HERO,
		full: API_SIZES.FULL
	};

	let requestedPixels = $derived(requestSize ?? apiSizes[size]);
	let remoteRequestSize = $derived(
		requestSize === 250 ? 'md' : requestSize === 500 ? 'lg' : requestSize === 1200 ? 'full' : size
	);
	let useRemoteUrl = $derived(remoteUrl && $imageSettingsStore.directRemoteImagesEnabled);
	let resolvedRemoteUrl = $derived(
		remoteUrl ? appendAudioDBSizeSuffix(remoteUrl, remoteRequestSize) : null
	);
	let responsiveRemoteUrl = $derived(
		remoteUrl && requestSize === 250 && responsiveSizes
			? appendAudioDBSizeSuffix(remoteUrl, 'lg')
			: null
	);
	let resolvedCustomUrl = $derived(customUrl === null ? null : getApiUrl(customUrl));

	let canonicalAlbumCoverUrl = $derived(
		imageType === 'album' && isValidMbid(mbid)
			? coverSrc('release-group', mbid, requestedPixels)
			: null
	);
	let validMbid = $derived(source === 'local' || imageType === 'album' ? true : isValidMbid(mbid));
	let hasSource = $derived(
		available &&
			((useRemoteUrl && resolvedRemoteUrl) ||
				(imageType === 'album' ? canonicalAlbumCoverUrl || resolvedCustomUrl || mbid : validMbid))
	);
	let apiEndpoint: CoverKind = $derived(imageType === 'album' ? 'release-group' : 'artist');
	let fallbackCoverUrl = $derived(coverSrc(apiEndpoint, mbid, requestedPixels));
	let responsiveCoverUrl = $derived(
		requestSize === 250 && responsiveSizes ? coverSrc(apiEndpoint, mbid, 500) : null
	);
	let coverUrl = $derived(
		imageType === 'album'
			? (canonicalAlbumCoverUrl ?? resolvedCustomUrl ?? fallbackCoverUrl)
			: fallbackCoverUrl
	);
	let displayCoverUrl = $derived(warmResolvedUrl ?? coverUrl);
	let displayCoverSrcset = $derived.by(() => {
		if (warmResolvedUrl || !responsiveSizes) return undefined;
		if (useRemoteUrl && resolvedRemoteUrl && responsiveRemoteUrl) {
			return `${resolvedRemoteUrl} 250w, ${responsiveRemoteUrl} 500w`;
		}
		return responsiveCoverUrl ? `${coverUrl} 250w, ${responsiveCoverUrl} 500w` : undefined;
	});
	let visualSourceKey = $derived(
		useRemoteUrl && resolvedRemoteUrl && !remoteError ? resolvedRemoteUrl : coverUrl
	);
	let sizeClasses = $derived(imageType === 'album' ? albumSizeClasses : artistSizeClasses);
	let sizeClass = $derived(sizeClasses[size]);
	let roundedClass = $derived(roundedClasses[rounded]);

	$effect(() => {
		const newKey = visualSourceKey;
		if (newKey !== retrySourceKey) {
			retrySourceKey = newKey;
			stopWatchingWarmCover?.();
			stopWatchingWarmCover = null;
			if (visualSettleTimer) clearTimeout(visualSettleTimer);
			visualSettleTimer = null;
			warmResolvedUrl = null;
			visualSettled = false;
			failed = false;
			if (imgError) {
				imgError = false;
				imgLoaded = false;
			}
		}
	});

	$effect(() => {
		if (hasSource && showPlaceholder && !imgLoaded && !visualSettled) {
			scheduleVisualSettlement();
		}
	});
	$effect(() => {
		// a stalled CDN image emits neither load nor error: at the settle deadline flip
		// to the covers proxy (cached image or 202 warm) instead of a dead placeholder
		if (useRemoteUrl && resolvedRemoteUrl && !remoteError && visualSettled && !imgLoaded) {
			remoteError = true;
		}
	});

	$effect(() => {
		const source =
			imageType === 'album' ? (canonicalAlbumCoverUrl ?? resolvedCustomUrl ?? mbid) : mbid;
		if (source && imgElement && source !== currentSource) {
			currentSource = source;
			imgError = false;
			imgLoaded = false;
			resetLazyImage(imgElement, displayCoverUrl, displayCoverSrcset);
		}
	});

	$effect(() => {
		remoteError = false;
		if (remoteUrl) imgLoaded = false;
	});

	function onRemoteError() {
		remoteError = true;
		imgLoaded = false;
	}

	function onImgError(event: Event) {
		imgError = true;
		imgLoaded = false;

		if (!retryOnError || warmResolvedUrl) {
			imgError = true;
			failed = true;
			visualSettled = true;
			return;
		}

		scheduleVisualSettlement();

		const selected = (event.currentTarget as HTMLImageElement).currentSrc;
		stopWatchingWarmCover ??= watchWarmingCover(currentCoverRequestUrl(selected), handleWarmUpdate);
	}

	function currentCoverRequestUrl(selected: string): string {
		if (!selected) return coverUrl;
		const parsed = new URL(selected, window.location.href);
		return `${parsed.pathname}${parsed.search}`;
	}

	function scheduleVisualSettlement() {
		if (visualSettleTimer) return;
		visualSettleTimer = setTimeout(() => {
			visualSettleTimer = null;
			visualSettled = true;
		}, COVER_VISUAL_SETTLE_MS);
	}

	function handleWarmUpdate(update: CoverWarmUpdate) {
		if (update.status === 'ready') {
			warmResolvedUrl = getApiUrl(update.url);
			imgError = false;
			failed = false;
			visualSettled = false;
			if (visualSettleTimer) clearTimeout(visualSettleTimer);
			visualSettleTimer = null;
		} else if (update.status === 'failed') {
			failed = true;
			visualSettled = true;
			if (visualSettleTimer) clearTimeout(visualSettleTimer);
			visualSettleTimer = null;
		}
	}

	function onImgLoad(e: Event) {
		const el = e.currentTarget as HTMLImageElement;
		// The lazy <img> mounts with a 1x1 data-URI gif before the IntersectionObserver swaps in
		// the real URL; that gif's load event must NOT mark the cover loaded, or imgLoaded flips
		// true and hides the shimmer skeleton while the cover is still warming (202 + poll).
		if (el.currentSrc.startsWith('data:')) return;
		if (visualSettleTimer) clearTimeout(visualSettleTimer);
		visualSettleTimer = null;
		imgLoaded = true;
		el.classList.remove('opacity-0');
		onload?.();
	}

	function bindImgElement(img: HTMLImageElement) {
		imgElement = img;
		return {
			destroy() {
				if (imgElement === img) {
					imgElement = null;
				}
			}
		};
	}

	onDestroy(() => {
		stopWatchingWarmCover?.();
		if (visualSettleTimer) clearTimeout(visualSettleTimer);
	});
</script>

<div
	class="relative overflow-hidden shrink-0 {transparentFallback
		? 'bg-transparent'
		: 'bg-base-200'} {sizeClass} {roundedClass} {className}"
>
	{#if showPlaceholder && (!imgLoaded || !hasSource)}
		{#if !hasSource || failed || visualSettled}
			<div
				class="absolute inset-0 w-full h-full flex items-center justify-center"
				data-testid="cover-fallback"
			>
				{#if imageType === 'album'}
					<Disc3 class="h-1/3 w-1/3 text-base-content/20" />
				{:else}
					<Users class="h-1/3 w-1/3 text-base-content/20" />
				{/if}
			</div>
		{:else}
			<div
				class="skeleton skeleton-shimmer absolute inset-0 w-full h-full"
				data-testid="cover-skeleton"
			></div>
		{/if}
	{/if}
	{#if useRemoteUrl && resolvedRemoteUrl && !remoteError}
		<img
			data-testid={testId}
			src={resolvedRemoteUrl}
			srcset={displayCoverSrcset}
			sizes={responsiveSizes}
			{alt}
			class="w-full h-full object-cover transition-opacity duration-300"
			class:opacity-0={!imgLoaded}
			referrerpolicy="no-referrer"
			loading={lazy ? 'lazy' : 'eager'}
			decoding="async"
			onerror={onRemoteError}
			onload={onImgLoad}
		/>
	{:else if hasSource && !imgError}
		{#if lazy}
			<img
				data-testid={testId}
				src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7"
				data-src={displayCoverUrl}
				data-srcset={displayCoverSrcset}
				sizes={responsiveSizes}
				{alt}
				class="w-full h-full object-cover opacity-0 transition-opacity duration-300"
				loading="lazy"
				decoding="async"
				use:lazyImage
				use:bindImgElement
				onerror={onImgError}
				onload={onImgLoad}
			/>
		{:else}
			<img
				data-testid={testId}
				src={displayCoverUrl}
				srcset={displayCoverSrcset}
				sizes={responsiveSizes}
				{alt}
				class="w-full h-full object-cover transition-opacity duration-300"
				class:opacity-0={!imgLoaded}
				loading="lazy"
				decoding="async"
				onerror={onImgError}
				onload={onImgLoad}
			/>
		{/if}
	{/if}
</div>
