<script lang="ts">
	import BaseImage from './BaseImage.svelte';
	import { albumArtworkPath } from '$lib/api/covers';

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
		albumId && coverVersion !== undefined ? albumArtworkPath(albumId, coverVersion) : null
	);
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
	customUrl={cachedLocalUrl ?? customUrl}
	{remoteUrl}
	{onload}
	{testId}
	source={cachedLocalUrl ? 'local' : source}
	{available}
	retryOnError={retryOnError ?? !cachedLocalUrl}
	transparentFallback={Boolean(cachedLocalUrl)}
	imageType="album"
/>
