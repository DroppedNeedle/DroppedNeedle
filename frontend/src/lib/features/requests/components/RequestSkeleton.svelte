<!-- A tab's loading rows, in the cards' shape. -->
<script lang="ts">
	interface Props {
		count?: number;
		/** Button placeholders on the right. */
		buttons?: number;
	}

	let { count = 3, buttons = 1 }: Props = $props();
</script>

<div class="flex flex-col gap-4" aria-busy="true">
	{#each Array(count) as _, i (`skeleton-${i}`)}
		<div class="rq-skel animate-pulse" style:animation-delay="{i * 100}ms">
			<div class="rq-skel__cover"></div>
			<div class="min-w-0 flex-1">
				<div class="rq-skel__bar h-6 w-56 max-w-full"></div>
				<div class="rq-skel__bar mt-3 h-4 w-28"></div>
				<div class="rq-skel__bar mt-3 h-3 w-72 max-w-full"></div>
			</div>
			<div class="rq-skel__side">
				{#each Array(buttons) as _, j (`button-${j}`)}
					<div class="rq-skel__bar h-9 w-32"></div>
				{/each}
			</div>
		</div>
	{/each}
</div>

<style>
	.rq-skel {
		display: flex;
		align-items: center;
		gap: 1.5rem;
		padding: 1.25rem 1.5rem;
		border: 1.5px solid var(--dn-line);
		border-radius: var(--radius-box);
		background-color: var(--color-base-200);
	}
	.rq-skel__cover {
		flex: none;
		width: 6.5rem;
		height: 6.5rem;
		border-radius: 0.375rem;
		background-color: var(--color-base-300);
	}
	.rq-skel__bar {
		border-radius: 0.25rem;
		background-color: var(--color-base-300);
	}
	.rq-skel__side {
		display: flex;
		gap: 0.625rem;
	}
	@media (max-width: 767px) {
		.rq-skel {
			gap: 0.875rem;
			padding: 0.875rem;
		}
		.rq-skel__cover {
			width: 4.5rem;
			height: 4.5rem;
		}
		.rq-skel__side {
			display: none;
		}
	}
</style>
