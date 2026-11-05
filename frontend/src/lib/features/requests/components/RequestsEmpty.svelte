<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { KitIcon } from '$lib/ui/Button.svelte';

	interface Props {
		icon: KitIcon;
		title: string;
		/** `success` for "all done" (an acid ring), `muted` for "nothing yet". */
		tone: 'success' | 'muted';
		children: Snippet;
		action?: Snippet;
	}

	let { icon: Icon, title, tone, children, action }: Props = $props();
</script>

<div class="rq-empty" class:rq-empty--muted={tone === 'muted'}>
	<span class="rq-empty__ring">
		<Icon class="size-8" aria-hidden="true" />
	</span>
	<h2 class="rq-empty__title">{title}</h2>
	<p class="rq-empty__text">{@render children()}</p>
	{@render action?.()}
</div>

<style>
	.rq-empty {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		min-height: 18rem;
		padding: 3.5rem 1.5rem;
		border: 1px solid var(--dn-line);
		border-radius: var(--radius-box);
		background-color: var(--color-base-200);
		text-align: center;
	}
	.rq-empty__ring {
		display: grid;
		place-items: center;
		width: 4.5rem;
		height: 4.5rem;
		margin-bottom: 1.25rem;
		border: 2px solid var(--dn-title);
		border-radius: 9999px;
		background-color: color-mix(in oklab, var(--dn-title) 10%, transparent);
		color: var(--dn-title);
	}
	.rq-empty--muted .rq-empty__ring {
		border-color: var(--dn-edge);
		background-color: var(--color-base-300);
		color: var(--dn-muted);
	}
	.rq-empty__title {
		font-family: var(--font-display);
		font-size: 1.875rem;
		font-weight: 400;
		line-height: 1.1;
		letter-spacing: 0.02em;
		text-transform: uppercase;
		color: var(--color-base-content);
	}
	.rq-empty__text {
		max-width: 26rem;
		margin-top: 0.625rem;
		font-size: 0.9375rem;
		color: var(--dn-muted);
	}
	@media (max-width: 639px) {
		.rq-empty {
			min-height: 14rem;
			padding: 2.5rem 1.25rem;
		}
		.rq-empty__title {
			font-size: 1.5rem;
		}
	}
</style>
