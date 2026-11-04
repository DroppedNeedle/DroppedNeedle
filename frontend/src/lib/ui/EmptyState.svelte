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

<div class="es-wrap">
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
		position: relative;
		display: flex;
		flex-direction: column;
		align-items: center;
		max-width: 32rem;
		margin-inline: auto;
		padding: 3.5rem 1.5rem;
		text-align: center;
		border-radius: var(--radius-box);
		border: 1px solid var(--color-line);
		background: var(--color-base-200);
	}

	.es-stamp {
		display: grid;
		place-items: center;
		width: 4.5rem;
		height: 4.5rem;
		margin-bottom: 1.25rem;
		border-radius: var(--radius-box);
		color: var(--color-muted);
		border: 1px solid var(--color-edge);
		background: var(--color-base-100);
	}

	.es-title {
		font-family: var(--font-display);
		font-weight: 400;
		font-size: 1.75rem;
		line-height: 1;
		letter-spacing: 0.01em;
		text-transform: uppercase;
		color: var(--color-title);
	}

	.es-desc {
		margin-top: 0.6rem;
		max-width: 28rem;
		font-size: 0.9375rem;
		line-height: 1.5;
		color: var(--color-muted);
	}
</style>
