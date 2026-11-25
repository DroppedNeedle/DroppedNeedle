<!--
	The slip after an answer: a strip of till paper over the receipt for
	eight seconds, with Undo and, after No, the other answers. A line along
	its foot runs down the time; focus or the pointer on it holds it up. On
	a phone it sits just above the No / Later / Keep bar.
-->
<script lang="ts">
	import { Undo2 } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import { fly } from '$lib/ui/motion';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { pickSlip } from '$lib/features/discover/state/passLine.svelte';

	const slip = $derived(pickSlip.current);
</script>

<div class="dn-slip-place" role="status">
	{#if slip}
		{#key slip.id}
			<div
				class="dn-slip"
				role="group"
				aria-label={slip.title}
				class:above-player={playerStore.isPlayerVisible}
				class:is-held={pickSlip.held}
				in:fly={{ y: 14, duration: 260 }}
				onpointerenter={() => pickSlip.hold()}
				onpointerleave={() => pickSlip.release()}
				onfocusin={() => pickSlip.hold()}
				onfocusout={(event) => {
					if (!event.currentTarget.contains(event.relatedTarget as Node | null)) pickSlip.release();
				}}
			>
				<div class="dn-slip__said">
					<strong>{slip.title}</strong>
					{#if slip.message}<span>{slip.message}</span>{/if}
				</div>
				{#if slip.action}
					{@const action = slip.action}
					<Button variant="paper" size="sm" icon={Undo2} onclick={() => action.run()}>
						{action.label}
					</Button>
				{/if}
				{#if slip.choices?.length}
					<div class="dn-slip__choices">
						<span class="dn-slip__ask">Or was it</span>
						{#each slip.choices as choice (choice.label)}
							<Button variant="paper" size="sm" onclick={() => choice.run()}>{choice.label}</Button>
						{/each}
					</div>
				{/if}
				<i class="dn-slip__time motion-keep" aria-hidden="true"></i>
			</div>
		{/key}
	{/if}
</div>

<style>
	/* always there, so what the slip says is announced; it lets the
	   receipt under it take the pointer while it holds nothing */
	.dn-slip-place {
		position: absolute;
		inset: 0;
		z-index: 5;
		pointer-events: none;
	}
	.dn-slip {
		position: absolute;
		inset: 0.375rem 0;
		pointer-events: auto;
		display: flex;
		align-items: center;
		gap: 0.625rem;
		padding: 0 0.75rem 0 1.125rem;
		overflow: hidden;
		background: var(--dn-paper-white);
		color: var(--dn-paper-content);
		box-shadow: 0 8px 14px -6px rgb(0 0 0 / 0.5);
		clip-path: polygon(
			0 0,
			100% 0,
			99.4% 12%,
			100% 25%,
			99.4% 37%,
			100% 50%,
			99.4% 62%,
			100% 75%,
			99.4% 88%,
			100% 100%,
			0 100%
		);
	}
	.dn-slip__said {
		display: flex;
		align-items: baseline;
		gap: 0.625rem;
		min-width: 0;
	}
	.dn-slip__said strong {
		font-size: 0.9375rem;
		white-space: nowrap;
	}
	.dn-slip__said span {
		overflow: hidden;
		font-size: 0.84375rem;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--dn-muted);
	}
	.dn-slip__choices {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		margin-left: auto;
	}
	.dn-slip__ask {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 700;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		white-space: nowrap;
		color: var(--dn-muted);
	}
	.dn-slip__time {
		position: absolute;
		left: 0;
		bottom: 0;
		width: 100%;
		height: 3px;
		background: var(--dn-paper-content);
		transform-origin: left;
		animation: dn-slip-time 8s linear both;
	}
	.dn-slip.is-held .dn-slip__time {
		animation-play-state: paused;
	}
	@keyframes dn-slip-time {
		to {
			transform: scaleX(0);
		}
	}

	@media (max-width: 639px) {
		.dn-slip {
			position: fixed;
			inset: auto 0.5rem calc(var(--ms-bottom-nav-offset, 0px) + 4.5rem);
			z-index: 31;
			flex-wrap: wrap;
			padding: 0.625rem 0.75rem 0.75rem;
			border-radius: 3px;
			box-shadow: 0 -6px 30px rgb(0 0 0 / 0.6);
			clip-path: none;
		}
		.dn-slip.above-player {
			bottom: calc(var(--ms-bottom-nav-offset, 0px) + var(--ms-player-height, 0px) + 4.5rem);
		}
		.dn-slip__said {
			flex: 1;
			flex-wrap: wrap;
		}
		.dn-slip__said span {
			white-space: normal;
		}
		.dn-slip__choices {
			flex-basis: 100%;
			margin-left: 0;
		}
		.dn-slip__ask {
			display: none;
		}
		.dn-slip__choices :global(.btn) {
			flex: 1;
			padding-inline: 0.375rem;
		}
	}
</style>
