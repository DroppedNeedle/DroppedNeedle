<script lang="ts">
	import { run } from 'svelte/legacy';

	import { onDestroy } from 'svelte';
	import { Disc3, Users } from 'lucide-svelte';
	import { lazyImage, resetLazyImage } from '$lib/utils/lazyImage';
	import { API_SIZES } from '$lib/constants';
	import { isValidMbid } from '$lib/utils/formatting';
	import { imageSettingsStore } from '$lib/stores/imageSettings';
	import { appendAudioDBSizeSuffix } from '$lib/utils/imageSuffix';
	import { getApiUrl } from '$lib/api/api-utils';

	interface Props {
		mbid: string;
		alt?: string;
		size?: 'xs' | 'sm' | 'md' | 'lg' | 'xl' | 'hero' | 'full';
		lazy?: boolean;
		showPlaceholder?: boolean;
		className?: string;
		rounded?: 'none' | 'sm' | 'md' | 'lg' | 'xl' | 'full';
		customUrl?: string | null;
		remoteUrl?: string | null;
		imageType?: 'album' | 'artist';
		onload?: () => void;
	}

	let {
		mbid,
		alt = 'Image',
		size = 'md',
		lazy = true,
		showPlaceholder = true,
		className = '',
		rounded = 'lg',
		customUrl = null,
		remoteUrl = null,
		imageType = 'album',
		onload = undefined
	}: Props = $props();

	// A cold cover comes back as 202 (warming) while it resolves in the background; the <img>
	// fires onerror, and we poll it in with a widening backoff (a rate-limited artist warm can
	// take a while) until it lands, holding a shimmer skeleton the whole time. Only after this
	// budget do we settle on the static placeholder.
	const MAX_RETRIES = 6;
	const RETRY_DELAYS = [1500, 3000, 5000, 8000, 12000, 20000];

	let imgError = $state(false);
	let failed = $state(false);
	let imgLoaded = $state(false);
	let remoteError = $state(false);
	let imgElement: HTMLImageElement | null = $state(null);
	let currentSource = $state('');
	let retryCount = $state(0);
	let retryTimer: ReturnType<typeof setTimeout> | null = $state(null);
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

	let useRemoteUrl = $derived(remoteUrl && $imageSettingsStore.directRemoteImagesEnabled);
	let resolvedRemoteUrl = $derived(remoteUrl ? appendAudioDBSizeSuffix(remoteUrl, size) : null);

	let canonicalAlbumCoverUrl = $derived(
		imageType === 'album' && isValidMbid(mbid)
			? getApiUrl(`/api/v1/covers/release-group/${mbid}?size=${apiSizes[size]}`)
			: null
	);
	let validMbid = $derived(imageType === 'artist' ? isValidMbid(mbid) : true);
	let hasSource = $derived(
		(useRemoteUrl && resolvedRemoteUrl) ||
			(imageType === 'album' ? canonicalAlbumCoverUrl || customUrl || mbid : validMbid)
	);
	let apiEndpoint = $derived(imageType === 'album' ? 'release-group' : 'artist');
	let fallbackCoverUrl = $derived(
		getApiUrl(`/api/v1/covers/${apiEndpoint}/${mbid}?size=${apiSizes[size]}`)
	);
	let coverUrl = $derived(
		imageType === 'album'
			? (canonicalAlbumCoverUrl ?? customUrl ?? fallbackCoverUrl)
			: fallbackCoverUrl
	);
	let retryCoverUrl = $derived(
		retryCount > 0 ? coverUrl + (coverUrl.includes('?') ? '&' : '?') + `_r=${retryCount}` : coverUrl
	);
	let sizeClasses = $derived(imageType === 'album' ? albumSizeClasses : artistSizeClasses);
	let sizeClass = $derived(sizeClasses[size]);
	let roundedClass = $derived(roundedClasses[rounded]);

	run(() => {
		const newKey = coverUrl;
		if (newKey !== retrySourceKey) {
			retrySourceKey = newKey;
			if (retryTimer) {
				clearTimeout(retryTimer);
				retryTimer = null;
			}
			retryCount = 0;
			failed = false;
			if (imgError) {
				imgError = false;
				imgLoaded = false;
			}
		}
	});

	run(() => {
		const source = imageType === 'album' ? (canonicalAlbumCoverUrl ?? customUrl ?? mbid) : mbid;
		if (source && imgElement && source !== currentSource) {
			currentSource = source;
			imgError = false;
			imgLoaded = false;
			resetLazyImage(imgElement, retryCoverUrl);
		}
	});

	run(() => {
		remoteError = false;
		if (remoteUrl) imgLoaded = false;
	});

	function onRemoteError() {
		remoteError = true;
		imgLoaded = false;
	}

	function onImgError() {
		if (retryCount < MAX_RETRIES) {
			// Hide the img so it re-creates and re-requests on the next tick; hold the skeleton
			// (imgLoaded false, failed false) until it either lands or we give up.
			imgError = true;
			imgLoaded = false;
			const delay = RETRY_DELAYS[retryCount] + Math.random() * 1000 - 500;
			retryTimer = setTimeout(() => {
				retryTimer = null;
				retryCount++;
				imgError = false;
				imgLoaded = false;
			}, delay);
		} else {
			imgError = true;
			failed = true;
		}
	}

	function onImgLoad(e: Event) {
		const el = e.currentTarget as HTMLImageElement;
		// The lazy <img> mounts with a 1x1 data-URI gif before the IntersectionObserver swaps in
		// the real URL; that gif's load event must NOT mark the cover loaded, or imgLoaded flips
		// true and hides the shimmer skeleton while the cover is still warming (202 + poll).
		if (el.currentSrc.startsWith('data:')) return;
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
		if (retryTimer) clearTimeout(retryTimer);
	});
</script>

<div class="relative overflow-hidden shrink-0 bg-base-200 {sizeClass} {roundedClass} {className}">
	{#if showPlaceholder && (!imgLoaded || !hasSource)}
		{#if !hasSource || failed}
			<div class="absolute inset-0 w-full h-full flex items-center justify-center">
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
			src={resolvedRemoteUrl}
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
				src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7"
				data-src={retryCoverUrl}
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
				src={retryCoverUrl}
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
