<script lang="ts">
	import BaseImage from './BaseImage.svelte';
	import { albumArtworkPath } from '$lib/api/covers';
	import { isValidMbid } from '$lib/utils/formatting';

	interface Props {
		mbid?: string;
		albumId?: string;
		coverVersion?: number;
		alt?: string;
		size?: 'xs' | 'sm' | 'md' | 'lg' | 'xl' | 'hero' | 'full';
		requestSize?: 250 | 500 | 1200;
		responsiveSizes?: string;
		lazy?: boolean;
		priority?: boolean;
		showPlaceholder?: boolean;
		className?: string;
		rounded?: 'none' | 'sm' | 'md' | 'lg' | 'xl' | 'full';
		customUrl?: string | null;
		remoteUrl?: string | null;
		onload?: () => void;
		testId?: string;
		source?: 'provider' | 'local';
		available?: boolean;
		/** The server says there is no art: draw the placeholder, ask for nothing. */
		artMissing?: boolean;
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
		priority = false,
		showPlaceholder = true,
		className = '',
		rounded = 'lg',
		customUrl = null,
		remoteUrl = null,
		onload = undefined,
		testId = undefined,
		source = 'provider',
		available = true,
		artMissing = false,
		retryOnError = undefined
	}: Props = $props();

	// `albumId` is a library album's local id. When `available` says it has art
	// of its own, that art is shown; otherwise `mbid` (its release group, when
	// linked) gives the release-group cover, and an unlinked album shows the
	// fallback. A local id is never sent where an MBID belongs. BaseImage builds
	// its srcset from release-group URLs, so local art goes without one. Art the
	// server knows is missing is never requested.
	let localArtUrl = $derived(
		albumId && available && !artMissing
			? albumArtworkPath(albumId, coverVersion, requestSize)
			: null
	);
</script>

<BaseImage
	mbid={localArtUrl ? '' : mbid}
	{alt}
	{size}
	{requestSize}
	responsiveSizes={localArtUrl ? undefined : responsiveSizes}
	{lazy}
	{priority}
	{showPlaceholder}
	{className}
	{rounded}
	customUrl={localArtUrl ?? customUrl}
	remoteUrl={artMissing ? null : remoteUrl}
	{onload}
	{testId}
	source={localArtUrl ? 'local' : source}
	available={artMissing ? false : albumId && !localArtUrl ? isValidMbid(mbid) : available}
	retryOnError={retryOnError ?? !localArtUrl}
	transparentFallback={Boolean(localArtUrl) && coverVersion !== undefined}
	imageType="album"
/>
