<!--
	The head of a settings section: a torn paper block with the section's
	Anton title, its one-line description and the section's own buttons
	(refresh, import, new) at the right. Inside, the sheet's theme applies, so
	the buttons are already measured for that stock. `children` holds a
	further line under the description (a link, a count).
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { themeStore } from '$lib/stores/theme.svelte';

	interface Props {
		title: string;
		description?: string;
		actions?: Snippet;
		children?: Snippet;
	}

	let { title, description, actions, children }: Props = $props();

	const theme = $derived(themeStore.resolved);
</script>

<header class="dn-set-head">
	<div
		class="dn-set-head__sheet dn-card-torn"
		data-theme={theme === 'midnight' ? 'midnight' : 'light'}
	>
		<div class="dn-set-head__text">
			<h2 class="dn-set-head__title">{title}</h2>
			{#if description}<p class="dn-set-head__lede">{description}</p>{/if}
			{@render children?.()}
		</div>
		{#if actions}
			<div class="dn-set-head__actions">{@render actions()}</div>
		{/if}
	</div>
</header>

<style>
	/* Read the page theme's paper here, before the sheet switches to its own theme. */
	.dn-set-head {
		--paper-bg: var(--dn-paper);
	}
	:global([data-theme='light']) .dn-set-head {
		filter: drop-shadow(0 6px 10px color-mix(in oklab, var(--dn-paper-content) 26%, transparent));
	}

	.dn-set-head__sheet {
		--tear: 0.625rem;
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 1rem 1.5rem;
		padding: 1.5rem 1.75rem 1.375rem;
		background-color: var(--paper-bg);
		color: var(--dn-paper-content);
	}

	.dn-set-head__text {
		flex: 1 1 22rem;
		min-width: 0;
	}

	.dn-set-head__title {
		font-family: var(--font-display);
		font-size: 2.75rem;
		font-weight: 400;
		line-height: 1;
		letter-spacing: 0.01em;
		text-transform: uppercase;
		overflow-wrap: anywhere;
		color: var(--dn-paper-content);
		font-synthesis: none;
	}

	.dn-set-head__lede {
		margin-top: 0.5rem;
		max-width: 68ch;
		font-size: 0.9375rem;
		line-height: 1.45;
		color: var(--dn-paper-content);
	}
	.dn-set-head__text :global(p + p) {
		margin-top: 0.375rem;
	}

	.dn-set-head__actions {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.625rem;
	}

	@media (max-width: 767px) {
		.dn-set-head__sheet {
			gap: 0.875rem;
			padding: 1.25rem 1rem 1.125rem;
		}
		.dn-set-head__text {
			flex-basis: 100%;
		}
		.dn-set-head__title {
			font-size: 2rem;
		}
		.dn-set-head__lede {
			font-size: 0.875rem;
		}
		.dn-set-head__actions {
			flex-wrap: nowrap;
			gap: 0.5rem;
		}
		.dn-set-head__actions :global(.btn) {
			--btn-p: 0.625rem;
			--fontsize: 0.9375rem;
			--size: 2.25rem;
			gap: 0.375rem;
			white-space: nowrap;
		}
	}

	@media (forced-colors: active) {
		.dn-set-head__sheet {
			border: 1px solid CanvasText;
		}
	}
</style>
