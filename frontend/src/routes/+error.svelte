<script lang="ts">
	import { Home, RotateCw } from 'lucide-svelte';
	import { page } from '$app/state';

	const status = $derived(page.status);
	const isNotFound = $derived(status === 404);
	const isBusy = $derived(status === 503);
	const heading = $derived(
		isNotFound ? 'Off the record' : isBusy ? 'Server busy' : 'Something broke'
	);
	const blurb = $derived(
		page.error?.message ??
			(isNotFound ? "That page isn't in the crate." : 'An unexpected error occurred.')
	);
</script>

<svelte:head>
	<title>{status} - DroppedNeedle</title>
</svelte:head>

<div class="err-wrap grain">
	<span class="err-code" aria-hidden="true">{status}</span>
	<h1 class="err-head">{heading}</h1>
	<p class="err-blurb">{blurb}</p>
	<div class="err-actions">
		<a href="/" class="btn btn-ghost btn-sm">
			<Home class="h-4 w-4" />
			Home
		</a>
		<button class="btn btn-accent btn-sm" onclick={() => location.reload()}>
			<RotateCw class="h-4 w-4" />
			Retry
		</button>
	</div>
</div>

<style>
	.err-wrap {
		--grain-opacity: 0.12;
		position: relative;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		min-height: 70vh;
		padding: 2rem 1.5rem;
		text-align: center;
	}

	.err-code {
		font-family: var(--font-display);
		font-weight: 800;
		font-size: clamp(6rem, 28vw, 16rem);
		line-height: 0.78;
		letter-spacing: -0.01em;
		color: oklch(from var(--color-base-content) l c h / 0.05);
		-webkit-text-stroke: 2px oklch(from var(--color-base-content) l c h / 0.18);
		text-shadow: 0 3px 2px oklch(from var(--color-base-content) 0 0 h / 0.45);
		user-select: none;
	}

	.err-head {
		margin-top: 1rem;
		font-family: var(--font-display);
		font-weight: 700;
		font-size: clamp(1.5rem, 5vw, 2.25rem);
		line-height: 1;
		letter-spacing: 0.02em;
		text-transform: uppercase;
		color: oklch(from var(--color-base-content) l c h / 0.9);
	}

	.err-blurb {
		margin-top: 0.65rem;
		max-width: 30rem;
		font-size: 0.9rem;
		line-height: 1.5;
		color: oklch(from var(--color-base-content) l c h / 0.6);
	}

	.err-actions {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		margin-top: 1.75rem;
	}
</style>
