<!--
	A transfer's progress: a mono line ("8 of 12 tracks · 389 MB / 572 MB")
	over an acid bar. The percent is read out, and drawn by the record beside.
-->
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
		if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(0)} MB`;
		return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
	}

	const facts = $derived(
		[
			filesTotal > 0 ? `${filesCompleted} of ${filesTotal} tracks` : null,
			bytesTotal > 0 ? `${fmtSize(bytesDownloaded)} / ${fmtSize(bytesTotal)}` : null
		].filter(Boolean)
	);
</script>

<div class="dl-progress" role="status" aria-live="polite" aria-label="Download progress">
	<p class="dl-progress__facts">
		<span class="sr-only">{clamped.toFixed(0)}%</span>
		{#each facts as fact, i (fact)}{#if i > 0}<span class="dl-progress__dot mx-1.5">·</span
				>{/if}{fact}{/each}
	</p>
	<div class="dl-track" aria-hidden="true">
		<div class="dl-fill" style:width="{clamped}%"></div>
	</div>
</div>

<style>
	.dl-progress {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}
	.dl-progress__facts {
		min-height: 1.25rem;
		font-family: var(--font-mono);
		font-size: 0.875rem;
		font-weight: 500;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		font-variant-numeric: tabular-nums;
	}
	.dl-progress__dot {
		color: var(--dn-muted);
	}
	.dl-track {
		height: 0.625rem;
		border-radius: 9999px;
		background: color-mix(in oklab, var(--color-base-content) 14%, transparent);
		overflow: hidden;
	}
	.dl-fill {
		height: 100%;
		border-radius: 9999px;
		background: var(--dn-action);
		box-shadow: inset 0 0 0 1px var(--dn-action-edge);
		transition: width 0.6s ease-out;
	}
	@media (prefers-reduced-motion: reduce) {
		.dl-fill {
			transition: none;
		}
	}
	@media (max-width: 639px) {
		.dl-progress__facts {
			font-size: 0.75rem;
			letter-spacing: 0.06em;
		}
	}
</style>
