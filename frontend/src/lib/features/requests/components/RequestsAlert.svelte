<script lang="ts">
	import { RefreshCw, TriangleAlert } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';

	interface Props {
		message: string;
		/** History's failure reads red, the other tabs' amber. */
		tone?: 'warning' | 'error';
		onretry: () => void;
	}

	let { message, tone = 'warning', onretry }: Props = $props();
</script>

<div role="alert" class="rq-alert" class:rq-alert--error={tone === 'error'}>
	<TriangleAlert class="size-5 shrink-0" aria-hidden="true" />
	<p class="min-w-0 flex-1 basis-56 text-sm text-base-content">{message}</p>
	<Button variant="outline" size="display-sm" icon={RefreshCw} onclick={onretry}>Retry</Button>
</div>

<style>
	.rq-alert {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem 1rem;
		margin-bottom: 1.25rem;
		padding: 0.75rem 1rem;
		border: 1.5px solid var(--color-warning);
		border-radius: var(--radius-box);
		background-color: color-mix(in oklab, var(--color-warning) 8%, transparent);
		color: var(--color-warning);
	}
	.rq-alert--error {
		border-color: var(--color-error);
		background-color: color-mix(in oklab, var(--color-error) 8%, transparent);
		color: var(--color-error);
	}
</style>
