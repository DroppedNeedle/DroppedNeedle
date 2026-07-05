<script lang="ts">
	import BaseImage from './BaseImage.svelte';

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
		onload?: () => void;
	}

	let {
		mbid,
		alt = 'Album',
		size = 'md',
		lazy = true,
		showPlaceholder = true,
		className = '',
		rounded = 'lg',
		customUrl = null,
		remoteUrl = null,
		onload = undefined
	}: Props = $props();
</script>

<BaseImage
	{mbid}
	{alt}
	{size}
	{lazy}
	{showPlaceholder}
	{className}
	{rounded}
	{customUrl}
	{remoteUrl}
	{onload}
	imageType="album"
/>
