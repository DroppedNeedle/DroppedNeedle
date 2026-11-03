<script lang="ts">
	interface Props {
		label?: string;
		className?: string;
	}

	let { label = 'Updating', className = '' }: Props = $props();
</script>

<span
	class="live-badge inline-flex items-center gap-2 rounded-full border border-primary/20 bg-primary/10 px-3 py-1 text-xs font-medium text-primary {className}"
	role="status"
	aria-label="{label} in the background"
>
	<span class="live-dot" aria-hidden="true">
		<span class="live-dot-core"></span>
		<span class="live-dot-ping"></span>
	</span>
	{label}
</span>

<style>
	.live-dot {
		position: relative;
		display: inline-flex;
		height: 0.5rem;
		width: 0.5rem;
		align-items: center;
		justify-content: center;
	}
	.live-dot-core {
		height: 0.5rem;
		width: 0.5rem;
		border-radius: 9999px;
		background: var(--color-primary);
	}
	.live-dot-ping {
		position: absolute;
		inset: 0;
		border-radius: 9999px;
		background: var(--color-primary);
		animation: live-ping 1.6s cubic-bezier(0, 0, 0.2, 1) infinite;
	}
	@keyframes live-ping {
		0% {
			transform: scale(1);
			opacity: 0.6;
		}
		75%,
		100% {
			transform: scale(2.4);
			opacity: 0;
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.live-dot-ping {
			animation: none;
			opacity: 0.25;
		}
	}
</style>
