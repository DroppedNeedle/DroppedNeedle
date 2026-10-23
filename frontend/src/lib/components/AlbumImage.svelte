<script lang="ts">
	import BaseImage from './BaseImage.svelte';
	import { API } from '$lib/constants';

	interface Props {
		mbid?: string;
		albumId?: string;
		coverVersion?: number;
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
		onload?: () => void;
		testId?: string;
		source?: 'provider' | 'local';
		available?: boolean;
		retryOnError?: boolean;
	}

	let {
		mbid = '',
		albumId = undefined,
		coverVersion = undefined,
		alt = 'Album',
		size = 'md',
		requestSize = undefined,
		responsiveSizes = undefined,
		lazy = true,
		showPlaceholder = true,
		className = '',
		rounded = 'lg',
		customUrl = null,
		remoteUrl = null,
		onload = undefined,
		testId = undefined,
		source = 'provider',
		available = true,
		retryOnError = undefined
	}: Props = $props();

	let cachedLocalUrl = $derived(
		albumId && coverVersion !== undefined
			? API.library.cachedAlbumArtwork(albumId, coverVersion)
			: null
	);
	// A local catalog ID can have embedded art before any provider identity exists.
	// The server resolves explicit provider fallback; local UUIDs are never provider IDs.
	let stableLocalUrl = $derived(
		source === 'local' && mbid
			? `/api/v1/library/albums/${encodeURIComponent(mbid)}/artwork/local`
			: null
	);
	let localUrl = $derived(cachedLocalUrl ?? stableLocalUrl);
</script>

<BaseImage
	{mbid}
	{alt}
	{size}
	{requestSize}
	{responsiveSizes}
	{lazy}
	{showPlaceholder}
	{className}
	{rounded}
	customUrl={localUrl ?? customUrl}
	remoteUrl={localUrl ? null : remoteUrl}
	{onload}
	{testId}
	source={localUrl ? 'local' : source}
	available={Boolean(localUrl) || available}
	retryOnError={retryOnError ?? !cachedLocalUrl}
	transparentFallback={Boolean(localUrl)}
	preferCustomUrl={Boolean(localUrl)}
	imageType="album"
/>
