<script lang="ts">
	import type { LibraryActivityItem } from '$lib/queries/library/LibraryOperationsTypes';

	interface Props {
		kind: 'scan' | 'identification';
		item?: LibraryActivityItem;
		compact?: boolean;
	}

	let { kind, item = undefined, compact = false }: Props = $props();

	const label = $derived(kind === 'scan' ? 'Local files' : 'Identification');
	const percentage = $derived(
		item?.total && item.total > 0 ? Math.min(100, (item.processed / item.total) * 100) : null
	);
	const status = $derived(
		item
			? kind === 'scan' && item.state === 'discovering'
				? item.processed
					? `${item.processed.toLocaleString()} files found`
					: 'Looking for files...'
				: item.indeterminate || item.total === null
					? `${item.processed.toLocaleString()} complete, total not known yet`
					: `${item.processed.toLocaleString()} of ${item.total.toLocaleString()}`
			: 'Idle'
	);
</script>

<div class="library-work-lane" class:library-work-lane--compact={compact} data-kind={kind}>
	<div class="flex min-w-0 items-baseline justify-between gap-3 text-xs">
		<span class="truncate font-semibold text-base-content/80">{label}</span>
		<span class="shrink-0 font-mono text-[0.7rem] text-base-content/60">{status}</span>
	</div>
	<div
		class="library-work-lane__track"
		class:library-work-lane__track--indeterminate={Boolean(item?.indeterminate)}
		class:library-work-lane__track--paused={item?.state === 'paused' || item?.state === 'pausing'}
		class:library-work-lane__track--failed={item?.state === 'failed'}
		role="progressbar"
		aria-label={`${label} progress`}
		aria-valuemin={item && !item.indeterminate ? 0 : undefined}
		aria-valuemax={item && !item.indeterminate && item.total !== null ? item.total : undefined}
		aria-valuenow={item && !item.indeterminate ? item.processed : undefined}
		aria-valuetext={status}
	>
		{#if percentage !== null && item}
			<span
				class="library-work-lane__fill"
				data-testid={`${kind}-progress-fill`}
				style={`width: ${percentage}%`}
			></span>
		{/if}
	</div>
</div>

<style>
	.library-work-lane {
		display: grid;
		gap: 0.2rem;
	}

	.library-work-lane__track {
		position: relative;
		height: 0.36rem;
		overflow: hidden;
		border-radius: 9999px;
		background: color-mix(in oklab, var(--color-base-content) 12%, transparent);
	}

	.library-work-lane__fill {
		display: block;
		height: 100%;
		border-radius: inherit;
		background: var(--color-library-index);
		transition: width 220ms ease-out;
	}

	.library-work-lane[data-kind='identification'] .library-work-lane__fill {
		background: var(--color-library-identify);
	}

	.library-work-lane__track--indeterminate::after {
		content: '';
		position: absolute;
		inset: 0;
		width: 42%;
		background: repeating-linear-gradient(
			110deg,
			transparent 0 0.35rem,
			color-mix(in oklab, var(--color-library-index) 52%, transparent) 0.35rem 0.7rem
		);
		animation: library-work-indeterminate 1.8s linear infinite;
	}

	.library-work-lane[data-kind='identification'] .library-work-lane__track--indeterminate::after {
		background: repeating-linear-gradient(
			110deg,
			transparent 0 0.35rem,
			color-mix(in oklab, var(--color-library-identify) 52%, transparent) 0.35rem 0.7rem
		);
	}

	.library-work-lane__track--paused::after {
		animation-play-state: paused;
	}

	.library-work-lane__track--failed {
		box-shadow: inset 0 0 0 1px var(--color-library-failed);
	}

	@media (prefers-reduced-motion: reduce) {
		/* a full, still bar in place of the moving stripe */
		.library-work-lane__track--indeterminate::after {
			transform: none !important;
			width: 100%;
		}
	}
</style>
