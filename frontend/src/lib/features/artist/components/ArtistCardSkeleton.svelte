<script lang="ts">
	import MediaTileSkeleton from '$lib/media/MediaTileSkeleton.svelte';

	interface Props {
		variant?: 'compact' | 'detailed';
	}

	let { variant = 'compact' }: Props = $props();
</script>

<MediaTileSkeleton kind="artist" class="w-full shrink-0 {variant === 'detailed' ? 'pb-6' : ''}" />
