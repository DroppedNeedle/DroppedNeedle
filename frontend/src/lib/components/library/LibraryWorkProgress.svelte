<script lang="ts">
	import type { LibraryWorkItem } from '$lib/queries/library/LibraryOperationsTypes';
	import {
		libraryWorkPercentage,
		libraryWorkPhase,
		libraryWorkProgress
	} from './LibraryWorkPresentation';

	interface Props {
		item: LibraryWorkItem;
		compact?: boolean;
	}

	let { item, compact = false }: Props = $props();
	const percentage = $derived(libraryWorkPercentage(item));
	const progress = $derived(libraryWorkProgress(item));
	const phase = $derived(libraryWorkPhase(item));
</script>

<div
	class="library-live-progress"
	class:library-live-progress--compact={compact}
	data-effect={item.effect}
>
	<div class="library-live-progress__copy">
		<span>{phase}</span>
		<strong>{progress}{percentage !== null ? ` · ${percentage}%` : ''}</strong>
	</div>
	<div
		class="library-live-progress__track"
		class:library-live-progress__track--indeterminate={percentage === null &&
			item.state !== 'paused' &&
			item.state !== 'failed'}
		class:library-live-progress__track--paused={item.state === 'paused'}
		role="progressbar"
		aria-label={`${phase}: ${progress}`}
		aria-valuemin={percentage !== null ? 0 : undefined}
		aria-valuemax={percentage !== null ? 100 : undefined}
		aria-valuenow={percentage ?? undefined}
		aria-valuetext={progress}
	>
		{#if percentage !== null}
			<span
				class="library-live-progress__fill"
				data-testid="library-work-progress-fill"
				style={`width: ${percentage}%`}
			></span>
		{/if}
	</div>
</div>

<style>
	.library-live-progress {
		display: grid;
		gap: 0.24rem;
	}

	.library-live-progress__copy {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 0.75rem;
		font-size: 0.7rem;
	}

	.library-live-progress__copy span {
		min-width: 0;
		overflow: hidden;
		color: color-mix(in oklab, var(--color-base-content) 58%, transparent);
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.library-live-progress__copy strong {
		flex: none;
		font-family: var(--font-mono);
		font-size: 0.65rem;
		font-weight: 650;
		color: color-mix(in oklab, var(--color-base-content) 68%, transparent);
	}

	.library-live-progress__track {
		position: relative;
		height: 0.38rem;
		overflow: hidden;
		border-radius: 9999px;
		background: color-mix(in oklab, var(--color-base-content) 11%, transparent);
	}

	.library-live-progress__fill {
		display: block;
		height: 100%;
		border-radius: inherit;
		background: var(--color-library-index);
		transition: width 220ms ease-out;
	}

	.library-live-progress[data-effect='file_writing'] .library-live-progress__fill {
		background: var(--color-library-manage);
	}

	.library-live-progress[data-effect='attention'] .library-live-progress__fill {
		background: var(--color-error);
	}

	.library-live-progress__track--indeterminate::after {
		position: absolute;
		inset: 0;
		width: 38%;
		border-radius: inherit;
		background: linear-gradient(
			90deg,
			transparent,
			color-mix(in oklab, var(--color-library-index) 72%, transparent),
			transparent
		);
		animation: library-work-indeterminate 1.8s ease-in-out infinite;
		content: '';
	}

	.library-live-progress[data-effect='file_writing']
		.library-live-progress__track--indeterminate::after {
		background: linear-gradient(
			90deg,
			transparent,
			color-mix(in oklab, var(--color-library-manage) 72%, transparent),
			transparent
		);
	}

	.library-live-progress__track--paused::after {
		animation-play-state: paused;
	}

	@media (max-width: 640px) {
		.library-live-progress__copy span {
			display: none;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		/* a full, still bar in place of the moving sheen */
		.library-live-progress__track--indeterminate::after {
			transform: none !important;
			width: 100%;
		}
	}
</style>
