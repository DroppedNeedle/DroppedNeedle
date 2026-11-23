<!--
	A cream divider tab standing up from a line across a bin, the way a
	record shop files its racks. The tab carries the group's name and
	nothing else ("AROUND POPPY"); the share, when there is one, sits apart
	at the right end of the line in quiet mono ("35%"). A screen reader
	hears the share as `shareSpoken` ("Around Poppy, 35% of your
	listening"). `offset` moves the tab along the line (rem), so a run of
	dividers can stagger; a parent sets the room above it as
	`--divider-space`.
-->
<script lang="ts">
	interface Props {
		name: string;
		share?: string;
		/** The share as it should be heard; `share` when not given. */
		shareSpoken?: string;
		offset?: number;
		/** Layout only: margin. */
		class?: string;
	}

	let { name, share, shareSpoken, offset = 0, class: className = '' }: Props = $props();
</script>

<p class="dn-divider {className}" style:--tab-x="{offset}rem">
	<span class="dn-divider__tab">{name}</span>
	{#if share}
		<span class="sr-only">, {shareSpoken ?? share}</span>
		<span class="dn-divider__share" aria-hidden="true">{share}</span>
	{/if}
</p>

<style>
	.dn-divider {
		position: relative;
		height: 1.75rem;
		margin: var(--divider-space, 1.125rem) 0 0.25rem;
	}
	.dn-divider::after {
		content: '';
		position: absolute;
		left: 0;
		right: 0;
		bottom: 0;
		height: 2px;
		border-radius: 1px;
		background-color: var(--dn-paper);
	}

	.dn-divider__tab {
		position: absolute;
		bottom: 0;
		left: var(--tab-x, 0);
		max-width: calc(100% - 4rem - var(--tab-x, 0rem));
		height: 1.75rem;
		line-height: 1.75rem;
		padding: 0 1.125rem;
		overflow: hidden;
		border-radius: 6px 6px 0 0;
		background-color: var(--dn-paper);
		color: var(--dn-paper-content);
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 700;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	/* paper on a pale box needs a hairline: round the tab, and as the line */
	:global([data-theme='light']) .dn-divider::after {
		height: 1px;
		background-color: var(--dn-paper-border);
	}
	:global([data-theme='light']) .dn-divider__tab {
		border: 1px solid var(--dn-paper-border);
		border-bottom: 0;
		line-height: calc(1.75rem - 1px);
	}

	.dn-divider__share {
		position: absolute;
		right: 0.125rem;
		bottom: 0.5rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		line-height: 1;
		letter-spacing: 0.12em;
		color: var(--dn-muted);
	}

	@media (forced-colors: active) {
		.dn-divider__tab {
			border: 1px solid CanvasText;
			border-bottom: 0;
		}
		.dn-divider::after {
			background-color: CanvasText;
		}
	}
</style>
