<script lang="ts">
	interface Props {
		size?: 'sm' | 'md';
		color?: string;
	}

	let { size = 'sm', color = 'bg-accent' }: Props = $props();

	const dims = $derived(
		size === 'md'
			? { gap: 'gap-0.5', w: 'w-0.5', h: 'h-4' }
			: { gap: 'gap-0.5', w: 'w-0.5', h: 'h-3.5' }
	);
</script>

<div class="flex items-end {dims.gap} {dims.h} justify-center" role="img" aria-label="Now playing">
	<span class="{dims.w} {color} animate-equalizer-1 rounded-full"></span>
	<span class="{dims.w} {color} animate-equalizer-2 rounded-full"></span>
	<span class="{dims.w} {color} animate-equalizer-3 rounded-full"></span>
</div>
