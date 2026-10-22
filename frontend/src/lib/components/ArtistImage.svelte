<script lang="ts">
	import BaseImage from './BaseImage.svelte';
	import { artistArtworkPath } from '$lib/api/covers';
	import { isValidMbid } from '$lib/utils/formatting';

	interface Props {
		mbid: string;
		alt?: string;
		size?: 'xs' | 'sm' | 'md' | 'lg' | 'xl' | 'hero' | 'full';
		requestSize?: 250 | 500 | 1200;
		responsiveSizes?: string;
		lazy?: boolean;
		priority?: boolean;
		showPlaceholder?: boolean;
		className?: string;
		rounded?: 'none' | 'sm' | 'md' | 'lg' | 'xl' | 'full';
		remoteUrl?: string | null;
		onload?: () => void;
		source?: 'provider' | 'local';
		available?: boolean;
		testId?: string;
		/**
		 * The artist's library id. An artist with no MusicBrainz link then
		 * shows the art of one of their own albums.
		 */
		localId?: string | null;
	}

	let {
		mbid,
		alt = 'Artist',
		size = 'md',
		requestSize = undefined,
		responsiveSizes = undefined,
		lazy = true,
		priority = false,
		showPlaceholder = true,
		className = '',
		rounded = 'full',
		remoteUrl = null,
		onload = undefined,
		source = 'provider',
		available = true,
		testId = undefined,
		localId = null
	}: Props = $props();

	let localArtUrl = $derived(
		!isValidMbid(mbid) && localId
			? artistArtworkPath(localId, requestSize ?? (size === 'xs' || size === 'sm' ? 250 : 500))
			: null
	);
</script>

<BaseImage
	{mbid}
	{alt}
	{size}
	{requestSize}
	responsiveSizes={localArtUrl ? undefined : responsiveSizes}
	{lazy}
	{priority}
	{showPlaceholder}
	{className}
	{rounded}
	{remoteUrl}
	{onload}
	customUrl={localArtUrl}
	source={localArtUrl ? 'local' : source}
	available={localArtUrl ? true : available}
	retryOnError={!localArtUrl}
	{testId}
	imageType="artist"
/>
