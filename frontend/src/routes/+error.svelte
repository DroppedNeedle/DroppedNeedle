<script lang="ts">
	import { House, RotateCw } from 'lucide-svelte';
	import { page } from '$app/state';
	import { withBasePath } from '$lib/utils/basePath';
	import Button from '$lib/ui/Button.svelte';

	const status = $derived(page.status);
	const isNotFound = $derived(status === 404);
	const kind = $derived(page.error?.kind);
	const heading = $derived(
		isNotFound
			? 'Off the record'
			: kind === 'busy'
				? 'Server busy'
				: kind === 'unreachable'
					? "Can't reach the server"
					: 'Something broke'
	);
	const blurb = $derived(
		page.error?.message ??
			(isNotFound ? "That page isn't in the crate." : 'An unexpected error occurred.')
	);
</script>

<svelte:head>
	<title>{status} - DroppedNeedle</title>
</svelte:head>

<div class="err-wrap">
	<span class="err-code" aria-hidden="true">{status}</span>
	<h1 class="err-head">{heading}</h1>
	<p class="err-blurb">{blurb}</p>
	<div class="err-actions">
		<Button variant="ghost" size="sm" href={withBasePath('/')} icon={House}>Home</Button>
		<Button variant="primary" size="sm" icon={RotateCw} onclick={() => location.reload()}>
			Retry
		</Button>
	</div>
</div>

<style>
	.err-wrap {
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
		font-size: clamp(6rem, 28vw, 16rem);
		line-height: 0.85;
		color: transparent;
		-webkit-text-stroke: 2px var(--color-edge);
		user-select: none;
	}

	.err-head {
		margin-top: 1rem;
		font-family: var(--font-display);
		font-size: clamp(1.75rem, 5vw, 2.5rem);
		line-height: 1;
		letter-spacing: 0.01em;
		text-transform: uppercase;
		color: var(--color-title);
	}

	.err-blurb {
		margin-top: 0.65rem;
		max-width: 30rem;
		font-size: 1rem;
		line-height: 1.5;
		color: var(--color-muted);
	}

	.err-actions {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		margin-top: 1.75rem;
	}
</style>
