<script lang="ts">
	import { getApiUrl } from '$lib/api/api-utils';

	interface Props {
		imageUrl: string | null;
		opacity?: number;
		hoverOpacity?: number;
		blur?: number;
		hoverBlur?: number;
		position?: 'right' | 'full';
		tintColor?: string;
	}

	let {
		imageUrl,
		opacity = 0.14,
		hoverOpacity = 0.2,
		blur = 2,
		hoverBlur = 1,
		position = 'right',
		tintColor
	}: Props = $props();

	let loaded = $state(false);

	$effect(() => {
		if (imageUrl) loaded = false;
	});
</script>

{#if imageUrl}
	<div
		class="hero-backdrop absolute inset-0 pointer-events-none overflow-hidden"
		aria-hidden="true"
		style:--hb-opacity={loaded ? opacity : 0}
		style:--hb-hover-opacity={loaded ? hoverOpacity : 0}
		style:--hb-blur="{blur}px"
		style:--hb-hover-blur="{hoverBlur}px"
	>
		<img
			src={getApiUrl(imageUrl)}
			alt=""
			loading="lazy"
			aria-hidden="true"
			onload={() => (loaded = true)}
			class="hero-backdrop-img absolute top-0 h-full object-cover transition-all duration-700 {position ===
			'right'
				? 'right-0 w-2/5'
				: 'inset-0 w-full'}"
		/>
		{#if tintColor}
			<div
				class="absolute inset-0"
				style:background={`linear-gradient(to right, ${tintColor}, oklch(from var(--color-base-100) l c h / 0.98) 55%, transparent)`}
			></div>
		{/if}
		<div
			class="absolute inset-0 bg-gradient-to-t from-base-100/90 via-transparent to-base-100/40"
		></div>
	</div>
{/if}

<style>
	.hero-backdrop-img {
		opacity: var(--hb-opacity, 0);
		filter: blur(var(--hb-blur, 2px));
	}

	:global(.group):hover .hero-backdrop-img {
		opacity: var(--hb-hover-opacity, 0.2);
		filter: blur(var(--hb-hover-blur, 1px));
		scale: 1.05;
	}

	@media (prefers-reduced-motion: reduce) {
		.hero-backdrop-img {
			transition: none !important;
		}
		:global(.group):hover .hero-backdrop-img {
			scale: 1;
		}
	}
</style>
