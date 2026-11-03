<script lang="ts">
	interface Props {
		percent?: number;
		bytesDownloaded?: number;
		bytesTotal?: number;
		filesCompleted?: number;
		filesTotal?: number;
	}
	let {
		percent = 0,
		bytesDownloaded = 0,
		bytesTotal = 0,
		filesCompleted = 0,
		filesTotal = 0
	}: Props = $props();

	const clamped = $derived(Math.max(0, Math.min(100, percent)));

	function fmtSize(bytes: number): string {
		if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
		if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
		return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
	}
</script>

<div class="flex flex-col gap-1" role="status" aria-live="polite" aria-label="Download progress">
	<div class="dl-track">
		<div class="dl-fill" style="width:{clamped}%"></div>
	</div>
	<div class="flex items-center justify-between text-[11px] text-base-content/50 tabular-nums">
		<span>{clamped.toFixed(0)}%</span>
		<span>
			{#if bytesTotal > 0}{fmtSize(bytesDownloaded)} / {fmtSize(bytesTotal)}{/if}
			{#if filesTotal > 0}<span class="text-base-content/30"> · </span>{filesCompleted}/{filesTotal} files{/if}
		</span>
	</div>
</div>

<style>
	.dl-track {
		height: 6px;
		border-radius: 3px;
		background: oklch(from var(--color-base-300) l c h);
		overflow: hidden;
	}
	.dl-fill {
		position: relative;
		height: 100%;
		border-radius: 3px;
		background: linear-gradient(
			90deg,
			oklch(from var(--color-primary) l c h / 0.7),
			oklch(from var(--color-primary) l c h)
		);
		transition: width 0.6s var(--ease-spring, ease-out);
	}
	.dl-fill::after {
		content: '';
		position: absolute;
		inset: 0;
		background: linear-gradient(
			90deg,
			transparent,
			oklch(from var(--color-primary) l c h / 0.28),
			transparent
		);
		animation: dl-sweep 2s ease-in-out infinite;
	}
	@keyframes dl-sweep {
		0% {
			transform: translateX(-100%);
		}
		100% {
			transform: translateX(100%);
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.dl-fill {
			transition: none;
		}
		.dl-fill::after {
			animation: none;
		}
	}
</style>
