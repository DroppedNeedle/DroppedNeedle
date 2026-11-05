<!--
	The frame of every signed-out page (sign-in, recovery, setup, the SSO
	return, the upgrade) and the app connect tab: the dot frame, a big black
	record with the "DN / CC 3.2" ticket peeking in at the top right, and
	the brand block (the mark, the DROPPEDNEEDLE wordmark as the page's h1,
	an acid rule and a spaced mono line) over the page's one card. `width`
	sets the column: `sm` for a sign-in form, `md` for recovery, `lg` for the
	upgrade's reports. The column sits in the middle of the screen while it
	fits and runs from the top once it is taller.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import BarcodeTicket from './BarcodeTicket.svelte';
	import FritFrame from './FritFrame.svelte';
	import Logo from './Logo.svelte';
	import VinylRecord from './VinylRecord.svelte';

	interface Props {
		/** The spaced mono line under the wordmark ("Sign in to continue"). */
		kicker: string;
		width?: 'sm' | 'md' | 'lg';
		children: Snippet;
	}

	let { kicker, width = 'sm', children }: Props = $props();
</script>

<div class="dn-auth">
	<FritFrame />
	<div class="dn-decor dn-auth__deck" aria-hidden="true">
		<VinylRecord sheen="acid" class="dn-auth__record" />
		<BarcodeTicket label="DN / CC 3.2" class="dn-auth__ticket" />
	</div>
	<main class="dn-auth__column" data-width={width}>
		<header class="dn-auth__brand">
			<Logo variant="icon" label="" class="dn-auth__mark" />
			<h1 class="dn-auth__wordmark">DroppedNeedle</h1>
			<span class="dn-decor dn-auth__rule" aria-hidden="true"></span>
			<p class="dn-auth__kicker">{kicker}</p>
		</header>
		{@render children()}
	</main>
</div>

<style>
	.dn-auth {
		position: relative;
		isolation: isolate;
		display: flex;
		min-height: 100dvh;
		overflow: clip;
		padding: 4.5rem 1.5rem 3.5rem;
		background: var(--color-base-100);
	}

	/* The record hangs off the top edge, its lower left in view, with the
	   ticket stuck across it. */
	.dn-auth__deck {
		--size: clamp(15rem, 34vw, 31rem);
		position: absolute;
		z-index: 0;
		top: calc(-0.56 * var(--size));
		right: calc(-0.2 * var(--size));
		width: var(--size);
		pointer-events: none;
	}
	.dn-auth__deck :global(.dn-auth__record) {
		--sheen-from: 165deg;
		width: 100%;
	}
	.dn-auth__deck :global(.dn-auth__ticket) {
		position: absolute;
		bottom: 9%;
		left: 4%;
		width: clamp(6.5rem, 9vw, 9rem);
		rotate: -14deg;
	}

	.dn-auth__column {
		position: relative;
		z-index: 1;
		display: flex;
		flex-direction: column;
		width: 100%;
		max-width: 27rem;
		margin: auto;
	}
	.dn-auth__column[data-width='md'] {
		max-width: 33rem;
	}
	.dn-auth__column[data-width='lg'] {
		max-width: 40rem;
	}

	.dn-auth__brand {
		display: flex;
		flex-direction: column;
		align-items: center;
		margin-bottom: 2.25rem;
		text-align: center;
	}
	.dn-auth__brand :global(.dn-auth__mark) {
		height: 2.75rem;
		margin-bottom: 0.875rem;
		color: var(--color-base-content);
	}
	.dn-auth__wordmark {
		font-family: var(--font-display);
		font-size: clamp(2.5rem, 12.5vw, 4.25rem);
		font-weight: 400;
		line-height: 0.9;
		letter-spacing: 0.02em;
		text-transform: uppercase;
		color: var(--color-base-content);
	}
	.dn-auth__rule {
		width: 4.5rem;
		height: 0.25rem;
		margin-block: 0.875rem 0.75rem;
		background: var(--dn-title);
	}
	.dn-auth__kicker {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 600;
		letter-spacing: 0.3em;
		text-transform: uppercase;
		color: var(--color-muted);
	}

	@media (max-width: 639px) {
		.dn-auth {
			padding: 5.5rem 1rem 2.5rem;
		}
		.dn-auth__deck {
			--size: 13.5rem;
			top: calc(-0.6 * var(--size));
			right: calc(-0.28 * var(--size));
		}
		.dn-auth__deck :global(.dn-auth__ticket) {
			bottom: 7%;
			left: 2%;
			width: 5.25rem;
		}
		.dn-auth__brand {
			margin-bottom: 1.75rem;
		}
		.dn-auth__brand :global(.dn-auth__mark) {
			height: 2.25rem;
		}
	}
</style>
