<script lang="ts">
	import type { ComponentType } from 'svelte';

	import { withBasePath } from '$lib/utils/basePath';
	import Button from '$lib/ui/Button.svelte';
	interface Props {
		icon: ComponentType;
		title: string;
		description?: string;
		ctaLabel?: string;
		ctaHref?: string;
	}

	let { icon, title, description, ctaLabel, ctaHref }: Props = $props();

	const SvelteComponent = $derived(icon);
</script>

<div class="es-wrap grain">
	<div class="es-stamp">
		<SvelteComponent class="h-8 w-8" strokeWidth={1.5} />
	</div>
	<h3 class="es-title">{title}</h3>
	{#if description}
		<p class="es-desc">{description}</p>
	{/if}
	{#if ctaLabel && ctaHref}
		<Button variant="primary" size="sm" href={withBasePath(ctaHref)} class="mt-5">{ctaLabel}</Button
		>
	{/if}
</div>

<style>
	.es-wrap {
		--grain-opacity: 0.1;
		position: relative;
		display: flex;
		flex-direction: column;
		align-items: center;
		max-width: 32rem;
		margin-inline: auto;
		padding: 3.5rem 1.5rem;
		text-align: center;
		border-radius: 1rem;
		border: 1px solid oklch(from var(--color-base-content) l c h / 0.08);
		background: oklch(from var(--color-base-100) l c h / 0.4);
	}

	.es-stamp {
		display: grid;
		place-items: center;
		width: 4.5rem;
		height: 4.5rem;
		margin-bottom: 1.25rem;
		border-radius: 1rem;
		color: oklch(from var(--color-base-content) l c h / 0.55);
		border: 1px solid oklch(from var(--color-base-content) l c h / 0.12);
		background: oklch(from var(--color-base-100) l c h / 0.6);
		box-shadow: inset 0 1px 0 oklch(from var(--color-base-content) l c h / 0.05);
	}

	.es-title {
		font-family: var(--font-display);
		font-weight: 700;
		font-size: 1.75rem;
		line-height: 1;
		letter-spacing: 0.02em;
		text-transform: uppercase;
		color: oklch(from var(--color-base-content) l c h / 0.9);
	}

	.es-desc {
		margin-top: 0.6rem;
		max-width: 28rem;
		font-size: 0.875rem;
		line-height: 1.5;
		color: oklch(from var(--color-base-content) l c h / 0.6);
	}
</style>
