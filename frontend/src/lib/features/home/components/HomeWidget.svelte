<script lang="ts">
	import { EyeOff, GripVertical } from 'lucide-svelte';
	import type { Snippet } from 'svelte';

	interface Props {
		id: string;
		label: string;
		editing?: boolean;
		onhide?: (id: string) => void;
		children: Snippet;
	}

	let { id, label, editing = false, onhide, children }: Props = $props();
</script>

<div class="home-widget" class:is-editing={editing} data-widget={id}>
	{#if editing}
		<div class="home-widget-bar">
			<span class="home-drag" title="Drag {label}">
				<GripVertical class="size-4" aria-hidden="true" />
			</span>
			<span class="home-widget-name">{label}</span>
			<button
				type="button"
				class="home-widget-hide"
				onclick={() => onhide?.(id)}
				aria-label="Hide {label}"
				title="Hide {label}"
			>
				<EyeOff class="size-4" aria-hidden="true" />
			</button>
		</div>
	{/if}
	<div class="home-widget-body">
		{@render children()}
	</div>
</div>

<style>
	.home-widget {
		position: relative;
		height: 100%;
		min-height: 0;
	}
	.home-widget-body {
		height: 100%;
		min-height: 0;
	}
	.home-widget.is-editing {
		outline: 1px dashed color-mix(in srgb, var(--color-base-content) 22%, transparent);
		outline-offset: 2px;
		border-radius: 0.25rem;
	}

	.home-widget-bar {
		position: absolute;
		top: 0;
		right: 0;
		z-index: 3;
		display: flex;
		align-items: center;
		gap: 0.25rem;
		padding: 0.125rem 0.25rem 0.125rem 0.375rem;
		border-bottom-left-radius: 0.375rem;
		background: color-mix(in srgb, var(--color-base-100) 88%, transparent);
		backdrop-filter: blur(2px);
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--color-base-content);
	}
	.home-drag {
		display: inline-flex;
		align-items: center;
		cursor: grab;
		touch-action: none;
		color: var(--color-base-content);
	}
	.home-drag:active {
		cursor: grabbing;
	}
	.home-widget-name {
		max-width: 14rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.home-widget-hide {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 1.75rem;
		height: 1.75rem;
		border-radius: 0.25rem;
		color: var(--color-base-content);
	}
	.home-widget-hide:hover {
		background: var(--dn-action);
		color: var(--dn-action-content);
	}
</style>
