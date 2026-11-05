<!--
	"Because You Listened" before there is enough history: a short wide
	panel with the section's title, a little stack of sleeves with paper
	labels (decoration), and the message.
-->
<script lang="ts">
	import { Heart } from 'lucide-svelte';
	import LiveUpdatingBadge from '$lib/ui/LiveUpdatingBadge.svelte';

	interface Props {
		updating?: boolean;
	}

	let { updating = false }: Props = $props();
</script>

<section class="dn-bempty" aria-labelledby="because-empty-title">
	<div class="dn-bempty__head">
		<Heart class="size-6 text-title" aria-hidden="true" />
		<h2 class="dn-panel__title">Because You Listened</h2>
		{#if updating}<LiveUpdatingBadge />{/if}
	</div>

	<div class="dn-decor dn-bempty__stack" aria-hidden="true">
		<span class="dn-bempty__sleeve dn-bempty__sleeve--back"></span>
		<span class="dn-bempty__sleeve dn-bempty__sleeve--mid"></span>
		<span class="dn-bempty__sleeve dn-bempty__sleeve--front"></span>
		<span class="dn-bempty__label dn-bempty__label--one">More music</span>
		<span class="dn-bempty__label dn-bempty__label--two">More recommendations</span>
	</div>

	<div class="dn-bempty__message">
		<span class="dn-bempty__mark" aria-hidden="true"><Heart class="size-6" /></span>
		<div>
			<h3 id="because-empty-title" class="dn-bempty__title">
				Listen to more music to see recommendations
			</h3>
			<p class="mt-2 text-sm text-muted">
				As you build up your listening history, we'll surface artists and albums based on what you
				love.
			</p>
		</div>
	</div>
</section>

<style>
	.dn-bempty {
		display: grid;
		grid-template-columns: minmax(11rem, auto) 15rem minmax(0, 1fr);
		align-items: center;
		gap: 2.5rem;
		padding: 1.75rem 2rem;
		border: 1px solid var(--dn-line);
		border-radius: var(--radius-box);
		background-color: var(--color-base-200);
	}

	.dn-bempty__head {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.75rem;
	}

	/* three sleeves fanned with a record edge peeking out, two paper labels */
	.dn-bempty__stack {
		position: relative;
		height: 8.5rem;
	}
	.dn-bempty__sleeve {
		position: absolute;
		top: 0.5rem;
		width: 6.5rem;
		aspect-ratio: 1;
		border-radius: 3px;
		box-shadow: 4px 8px 18px -8px rgb(0 0 0 / 0.7);
	}
	.dn-bempty__sleeve--back {
		left: 1rem;
		background:
			radial-gradient(circle at 100% 50%, var(--dn-vinyl) 0 45%, transparent 46%),
			var(--color-neutral);
		rotate: -9deg;
	}
	.dn-bempty__sleeve--mid {
		left: 4rem;
		background-color: var(--color-base-300);
		box-shadow:
			inset 0 0 0 1px var(--dn-line),
			4px 8px 18px -8px rgb(0 0 0 / 0.7);
		rotate: 3deg;
	}
	.dn-bempty__sleeve--front {
		left: 7.25rem;
		background:
			radial-gradient(circle, var(--dn-vinyl) 0 4%, transparent 4.5%),
			radial-gradient(circle, var(--dn-action) 0 22%, transparent 22.5%),
			repeating-radial-gradient(
				circle,
				var(--dn-vinyl) 0 2px,
				color-mix(in oklab, var(--dn-vinyl-groove) 60%, var(--dn-vinyl)) 2px 3px
			);
		border-radius: 9999px;
		rotate: 0deg;
	}
	.dn-bempty__label {
		position: absolute;
		padding: 0.25rem 0.625rem;
		background-color: var(--dn-paper);
		color: var(--dn-paper-content);
		font-family: var(--font-mono);
		font-size: 0.625rem;
		font-weight: 700;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		white-space: nowrap;
		box-shadow: 0 1px 3px rgb(0 0 0 / 0.3);
	}
	.dn-bempty__label--one {
		top: 0;
		left: 0;
		rotate: -6deg;
	}
	.dn-bempty__label--two {
		bottom: 0.25rem;
		left: 2.5rem;
		rotate: 3deg;
	}

	.dn-bempty__message {
		display: flex;
		align-items: flex-start;
		gap: 1.25rem;
	}
	.dn-bempty__mark {
		display: grid;
		flex: none;
		place-items: center;
		width: 3.5rem;
		height: 3.5rem;
		border: 1.5px solid var(--dn-title);
		border-radius: 0.625rem;
		color: var(--dn-title);
	}
	.dn-bempty__title {
		font-family: var(--font-display);
		font-weight: 400;
		font-size: 1.75rem;
		line-height: 1;
		text-transform: uppercase;
		color: var(--dn-title);
		font-synthesis: none;
	}

	@media (max-width: 1279px) {
		.dn-bempty {
			grid-template-columns: minmax(0, 1fr) minmax(0, 1.4fr);
		}
		.dn-bempty__stack {
			display: none;
		}
	}

	@media (max-width: 767px) {
		.dn-bempty {
			grid-template-columns: minmax(0, 1fr);
			gap: 1.25rem;
			padding: 1.25rem;
		}
		.dn-bempty__head {
			flex-direction: row;
			flex-wrap: wrap;
			align-items: center;
		}
		.dn-bempty__title {
			font-size: 1.375rem;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.dn-bempty__sleeve,
		.dn-bempty__label {
			rotate: none;
		}
	}
</style>
